use eth::contracts::{FLUSH_EVENT_SIGNATURE, TRANSFER_EVENT_SIGNATURE};
use ethabi::Token;
use extensions::removed::{CancelRemoved, ExitOnLogRemoved};
use extensions::timeout::SubscriptionState;
use lru::LruCache;
use relay::{Network, NetworkType, TransferApprovalState};
use relay_config::logger::flush;
use server::handler::{BalanceOf, BalanceQuery};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{PoisonError, RwLockWriteGuard};
use std::time::Instant;
use std::{cmp, time};
use tokio_core::reactor;
use transfers::transfer::Transfer;
use web3::confirm::{wait_for_transaction_confirmation, SendTransactionWithConfirmation};
use web3::contract::tokens::{Detokenize, Tokenize};
use web3::contract::{ErrorKind, Options, QueryResult};
use web3::futures::future::join_all;
use web3::futures::prelude::*;
use web3::futures::sync::mpsc;
use web3::futures::try_ready;
use web3::helpers::CallFuture;
use web3::types::{Address, BlockNumber, Bytes, FilterBuilder, Log, Transaction, TransactionReceipt, H256, U256};
use web3::{contract, DuplexTransport, Transport};

enum ProcessFlushState<T: DuplexTransport + 'static> {
    Wait,
    CheckBalances(CheckBalances<T>),
    FilterContracts(FilterContracts),
    FilterLowBalance(FilterLowBalance),
    WithdrawWallets(Box<Future<Item = Vec<()>, Error = ()>>),
    WithdrawLeftovers(WithdrawLeftovers<T>),
}

pub struct ProcessFlush<T: DuplexTransport + 'static> {
    state: ProcessFlushState<T>,
    source: Network<T>,
    target: Network<T>,
    rx: mpsc::UnboundedReceiver<(Log, TransactionReceipt)>,
    handle: reactor::Handle,
    flush_receipt: Option<TransactionReceipt>,
}

impl<T: DuplexTransport + 'static> ProcessFlush<T> {
    pub fn new(
        source: &Network<T>,
        target: &Network<T>,
        rx: mpsc::UnboundedReceiver<(Log, TransactionReceipt)>,
        handle: &reactor::Handle,
    ) -> Self {
        ProcessFlush {
            state: ProcessFlushState::Wait,
            source: source.clone(),
            target: target.clone(),
            rx,
            handle: handle.clone(),
            flush_receipt: None,
        }
    }
}

impl<T: DuplexTransport + 'static> Future for ProcessFlush<T> {
    type Item = ();
    type Error = ();
    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        let network_type = self.source.network_type;
        let target = self.target.clone();
        let source = self.source.clone();
        loop {
            let flush_receipt = self.flush_receipt.clone();
            let next: ProcessFlushState<T> = match self.state {
                ProcessFlushState::Wait => {
                    let event = try_ready!(self.rx.poll());
                    match event {
                        Some((log, receipt)) => {
                            let removed = log.removed.unwrap_or(false);
                            if removed {
                                ProcessFlushState::Wait
                            } else {
                                if receipt.block_hash.is_none() {
                                    error!("Failed to get block hash for flush");
                                    return Err(());
                                }

                                if receipt.block_number.is_none() {
                                    error!("Failed to get block hash for flush");
                                    return Err(());
                                }

                                self.flush_receipt = Some(receipt.clone());
                                let balance_future = CheckBalances::new(&source, receipt.block_number);
                                ProcessFlushState::CheckBalances(balance_future)
                            }
                        }
                        None => {
                            return Ok(Async::Ready(()));
                        }
                    }
                }
                ProcessFlushState::CheckBalances(ref mut future) => {
                    let balances = try_ready!(future.poll());
                    let contract_future = FilterContracts::new(&source, balances);
                    ProcessFlushState::FilterContracts(contract_future)
                }
                ProcessFlushState::FilterContracts(ref mut future) => {
                    let balances = try_ready!(future.poll());
                    let low_balance_future = FilterLowBalance::new(&target, balances);
                    ProcessFlushState::FilterLowBalance(low_balance_future)
                }
                ProcessFlushState::FilterLowBalance(ref mut future) => {
                    let balances = try_ready!(future.poll());
                    match flush_receipt {
                        Some(receipt) => {
                            let futures: Vec<Box<Future<Item = (), Error = ()>>> = balances
                                .iter()
                                .filter_map(|(address, balance)| {
                                    let transfer =
                                        Transfer::from_receipt(address.clone(), balance.clone(), false, &receipt);
                                    match transfer {
                                        Ok(t) => Some(t.approve_withdrawal(&source, &target)),
                                        Err(e) => {
                                            error!(
                                                "error creating transfer to {} for {} nct from receipt: {:?}",
                                                address, balance, e
                                            );
                                            None
                                        }
                                    }
                                })
                                .collect();

                            ProcessFlushState::WithdrawWallets(Box::new(join_all(futures)))
                        }
                        None => {
                            error!("No flush receipt available");
                            return Err(());
                        }
                    }
                }
                ProcessFlushState::WithdrawWallets(ref mut future) => {
                    try_ready!(future.poll());
                    match flush_receipt {
                        Some(receipt) => {
                            ProcessFlushState::WithdrawLeftovers(WithdrawLeftovers::new(&source, &target, &receipt))
                        }
                        None => {
                            error!("No flush receipt available");
                            return Err(());
                        }
                    }
                }
                ProcessFlushState::WithdrawLeftovers(ref mut future) => {
                    try_ready!(future.poll());
                    info!("finished flush");
                    return Ok(Async::Ready(()));
                }
            };
            self.state = next;
        }
    }
}

#[derive(Debug, Clone)]
pub struct FeeWallet(Address);

impl Detokenize for FeeWallet {
    /// Creates a new instance from parsed ABI tokens.
    fn from_tokens(tokens: Vec<Token>) -> Result<Self, contract::Error>
    where
        Self: Sized,
    {
        let fee_wallet = tokens[0].clone().to_address().ok_or_else(|| {
            contract::Error::from_kind(contract::ErrorKind::Msg(
                "cannot parse fee wallet from contract response".to_string(),
            ))
        })?;
        debug!("fees wallet: {:?}", fee_wallet);
        Ok(FeeWallet(fee_wallet))
    }
}

pub struct FeeWalletQuery {}

impl FeeWalletQuery {
    fn new() -> Self {
        FeeWalletQuery {}
    }
}

impl Tokenize for FeeWalletQuery {
    fn into_tokens(self) -> Vec<Token> {
        vec![]
    }
}

enum WithdrawLeftoversState {
    GetBalance(Box<Future<Item = BalanceOf, Error = ()>>),
    GetFeeWallet(Box<Future<Item = FeeWallet, Error = ()>>),
    Withdraw(Box<Future<Item = (), Error = ()>>),
}

pub struct WithdrawLeftovers<T: DuplexTransport + 'static> {
    state: WithdrawLeftoversState,
    source: Network<T>,
    target: Network<T>,
    receipt: TransactionReceipt,
    balance: Option<U256>,
    fee_wallet: Option<Address>,
}

impl<T: DuplexTransport + 'static> WithdrawLeftovers<T> {
    fn new(source: &Network<T>, target: &Network<T>, flush_receipt: &TransactionReceipt) -> Self {
        let state = WithdrawLeftoversState::GetBalance(WithdrawLeftovers::get_balance(&target));
        WithdrawLeftovers {
            state,
            source: source.clone(),
            target: target.clone(),
            receipt: flush_receipt.clone(),
            balance: None,
            fee_wallet: None,
        }
    }

    fn get_balance(target: &Network<T>) -> Box<Future<Item = BalanceOf, Error = ()>> {
        let relay_contract_balance_query = BalanceQuery::new(target.relay.address());
        let target = target.clone();
        Box::new(
            target
                .token
                .query::<BalanceOf, Address, BlockNumber, BalanceQuery>(
                    "balanceOf",
                    relay_contract_balance_query,
                    target.account,
                    Options::default(),
                    BlockNumber::Latest,
                )
                .map_err(|e| {
                    error!("error retrieving contract balance: {:?}", e);
                    ()
                }),
        )
    }

    fn get_fee_wallet(target: &Network<T>) -> Box<Future<Item = FeeWallet, Error = ()>> {
        let fee_wallet_query = FeeWalletQuery::new();
        let target = target.clone();
        Box::new(
            target
                .token
                .query::<FeeWallet, Address, BlockNumber, FeeWalletQuery>(
                    "feeWallet",
                    fee_wallet_query,
                    target.account,
                    Options::default(),
                    BlockNumber::Latest,
                )
                .map_err(|e| {
                    error!("error retrieving fee wallet: {:?}", e);
                    ()
                }),
        )
    }
}

impl<T: DuplexTransport + 'static> Future for WithdrawLeftovers<T> {
    type Item = ();
    type Error = ();
    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        loop {
            let source = self.source.clone();
            let target = self.target.clone();
            let receipt = self.receipt.clone();
            let next = match self.state {
                WithdrawLeftoversState::GetBalance(ref mut future) => {
                    let balance = try_ready!(future.poll());
                    self.balance = Some(balance.0);
                    WithdrawLeftoversState::GetFeeWallet(WithdrawLeftovers::get_fee_wallet(&target))
                }
                WithdrawLeftoversState::GetFeeWallet(ref mut future) => {
                    let address = try_ready!(future.poll());
                    match self.balance {
                        Some(b) => match Transfer::from_receipt(address.0, b, false, &receipt) {
                            Ok(transfer) => {
                                WithdrawLeftoversState::Withdraw(transfer.approve_withdrawal(&source, &target))
                            }
                            Err(e) => {
                                error!("Error creating transaction from flush receipt: {:?}", e);
                                return Err(());
                            }
                        },
                        None => {
                            error!("error balance is none");
                            return Err(());
                        }
                    }
                }
                WithdrawLeftoversState::Withdraw(ref mut future) => {
                    try_ready!(future.poll());
                    return Ok(Async::Ready(()));
                }
            };
            self.state = next;
        }
    }
}

#[derive(Debug, Clone)]
pub struct Fees(U256);

impl Detokenize for Fees {
    /// Creates a new instance from parsed ABI tokens.
    fn from_tokens(tokens: Vec<Token>) -> Result<Self, contract::Error>
    where
        Self: Sized,
    {
        let fee = tokens[0].clone().to_uint().ok_or_else(|| {
            contract::Error::from_kind(contract::ErrorKind::Msg(
                "cannot parse fees from contract response".to_string(),
            ))
        })?;
        debug!("fees: {:?}", fee);
        Ok(Fees(fee))
    }
}

pub struct FeeQuery {}

impl FeeQuery {
    fn new() -> Self {
        FeeQuery {}
    }
}

impl Tokenize for FeeQuery {
    fn into_tokens(self) -> Vec<Token> {
        vec![]
    }
}

pub struct FilterLowBalance {
    future: Box<Future<Item = Fees, Error = ()>>,
    balances: Vec<(Address, U256)>,
}

impl FilterLowBalance {
    pub fn new<T: DuplexTransport + 'static>(target: &Network<T>, balances: Vec<(Address, U256)>) -> Self {
        // Get contract data
        let future = target
            .relay
            .query::<Fees, Address, BlockNumber, FeeQuery>(
                "fees",
                FeeQuery::new(),
                target.account.clone(),
                Options::default(),
                BlockNumber::Latest,
            )
            .map_err(|e| {
                error!("error getting relay fees: {:?}", e);
            });

        FilterLowBalance {
            future: Box::new(future),
            balances,
        }
    }
}

impl Future for FilterLowBalance {
    type Item = Vec<(Address, U256)>;
    type Error = ();
    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        let fee = try_ready!(self.future.poll());
        let fee_value = fee.0;
        let filtered = self
            .balances
            .iter()
            .filter_map(|(address, balance)| {
                if *balance > fee_value {
                    Some((*address, *balance))
                } else {
                    None
                }
            })
            .collect();
        Ok(Async::Ready(filtered))
    }
}

pub struct FilterContracts {
    future: Box<Future<Item = Vec<Bytes>, Error = ()>>,
    wallets: Vec<(Address, U256)>,
}

impl FilterContracts {
    pub fn new<T: DuplexTransport + 'static>(source: &Network<T>, wallets: Vec<(Address, U256)>) -> Self {
        // Get contract data
        // For whatever reason, doing this as an iter().map().collect() did not work
        let mut futures = Vec::new();
        for (address, balance) in wallets.clone() {
            let future = source.web3.eth().code(address.clone(), None).map_err(move |e| {
                error!("error retrieving code for wallet {}: {:?}", address, e);
            });
            futures.push(Box::new(future));
        }

        FilterContracts {
            future: Box::new(join_all(futures)),
            wallets: wallets.clone(),
        }
    }
}

impl Future for FilterContracts {
    type Item = Vec<(Address, U256)>;
    type Error = ();
    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        let all_bytes = try_ready!(self.future.poll());
        let filtered = self
            .wallets
            .iter()
            .zip(all_bytes)
            .filter_map(move |((address, balance), ref bytes)| {
                if bytes.0.len() == 0 {
                    Some((*address, *balance))
                } else {
                    None
                }
            })
            .collect();
        Ok(Async::Ready(filtered))
    }
}

pub enum CheckBalancesState {
    GetEndingBlock(Box<Future<Item = U256, Error = ()>>),
    GetLogWindow(u64, u64, Box<Future<Item = Vec<Log>, Error = ()>>),
}

pub struct CheckBalances<T: DuplexTransport + 'static> {
    source: Network<T>,
    state: CheckBalancesState,
    balances: HashMap<Address, U256>,
}

impl<T: DuplexTransport + 'static> CheckBalances<T> {
    fn new(source: &Network<T>, block: Option<U256>) -> Self {
        let state = match block {
            Some(b) => {
                let window_end = cmp::min(b.as_u64(), 1000);
                CheckBalancesState::GetLogWindow(
                    b.as_u64(),
                    window_end,
                    CheckBalances::build_next_window(source, 0, window_end),
                )
            }
            None => {
                let future = source.web3.eth().block_number().map_err(move |e| {
                    error!("error getting block number {:?}", e);
                });
                CheckBalancesState::GetEndingBlock(Box::new(future))
            }
        };
        CheckBalances {
            source: source.clone(),
            state,
            balances: HashMap::new(),
        }
    }

    fn build_next_window(source: &Network<T>, start: u64, end: u64) -> Box<Future<Item = Vec<Log>, Error = ()>> {
        let token_address: Address = source.token.address();
        let filter = FilterBuilder::default()
            .address(vec![token_address])
            .from_block(BlockNumber::from(start))
            .to_block(BlockNumber::Number(end))
            .topics(Some(vec![TRANSFER_EVENT_SIGNATURE.into()]), None, None, None)
            .build();
        //self.state = ;
        let future = source.web3.eth().logs(filter).map_err(move |e| {
            error!("error getting block number {:?}", e);
        });
        Box::new(future)
    }
}

impl<T: DuplexTransport + 'static> Future for CheckBalances<T> {
    type Item = Vec<(Address, U256)>;
    type Error = ();
    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        let source = self.source.clone();
        loop {
            let next = match self.state {
                CheckBalancesState::GetEndingBlock(ref mut future) => {
                    let block = try_ready!(future.poll());
                    let window_end = cmp::min(block.as_u64(), 1000);
                    let future = CheckBalances::build_next_window(&source, 0, window_end);
                    CheckBalancesState::GetLogWindow(block.as_u64(), window_end, future)
                }
                CheckBalancesState::GetLogWindow(end, window_end, ref mut future) => {
                    let logs = try_ready!(future.poll());
                    info!(
                        "found {} logs with transfers between {} and {}",
                        logs.len(),
                        window_end,
                        end
                    );
                    // Process existing logs
                    logs.iter().for_each(|log| {
                        if Some(true) != log.removed {
                            let sender_address: Address = log.topics[1].into();
                            let receiver_address: Address = log.topics[2].into();
                            let amount: U256 = log.data.0[..32].into();
                            debug!("{} transferred {} to {}", sender_address, amount, receiver_address);
                            // Don't care if source doesn't exist, because it is likely a mint in that case
                            let zero = U256::zero();
                            self.balances
                                .entry(sender_address)
                                .and_modify(|v| {
                                    if !v.is_zero() {
                                        *v -= amount;
                                    }
                                })
                                .or_insert(zero);
                            let dest_balance = self.balances.entry(receiver_address).or_insert(zero);
                            *dest_balance += amount;
                        }
                    });

                    debug!("Window end is {} of {} blocks", window_end, end);
                    // Setup next window
                    if window_end < end {
                        let next_window_end = cmp::min(end, window_end + 1000);
                        let future = CheckBalances::build_next_window(&source, window_end + 1, next_window_end);
                        CheckBalancesState::GetLogWindow(end, next_window_end, future)
                    } else {
                        return Ok(Async::Ready(
                            self.balances.iter().map(|(key, value)| (*key, *value)).collect(),
                        ));
                    }
                }
            };
            self.state = next;
        }
    }
}
