use eth::contracts::{FLUSH_EVENT_SIGNATURE, TRANSFER_EVENT_SIGNATURE};
use ethabi::Token;
use relay::Network;
use server::handler::{BalanceOf, BalanceQuery};
use std::cmp;
use std::collections::HashMap;
use transfers::transfer::Transfer;
use web3::contract::tokens::{Detokenize, Tokenize};
use web3::contract::Options;
use web3::futures::future::join_all;
use web3::futures::prelude::*;
use web3::futures::sync::mpsc;
use web3::futures::try_ready;
use web3::types::{Address, BlockNumber, Bytes, FilterBuilder, Log, TransactionReceipt, U256};
use web3::{contract, DuplexTransport};

/// struct for taking the FlushBlockQuery  and parsing the Tokens
#[derive(Debug, Clone)]
pub struct FlushBlock(U256);

impl Detokenize for FlushBlock {
    /// Creates a new instance from parsed ABI tokens.
    fn from_tokens(tokens: Vec<Token>) -> Result<Self, contract::Error>
    where
        Self: Sized,
    {
        let block = tokens[0].clone().to_uint().ok_or_else(|| {
            contract::Error::from_kind(contract::ErrorKind::Msg(
                "cannot parse flush blockfrom contract response".to_string(),
            ))
        })?;
        debug!("flush block: {:?}", block);
        Ok(FlushBlock(block))
    }
}

/// Query for getting the current FlushBlock from the relay contract
pub struct FlushBlockQuery {}

impl FlushBlockQuery {
    fn new() -> Self {
        FlushBlockQuery {}
    }
}

impl Tokenize for FlushBlockQuery {
    fn into_tokens(self) -> Vec<Token> {
        vec![]
    }
}

enum CheckForPastFlushState {
    CheckFlushBlock(Box<Future<Item = FlushBlock, Error = ()>>),
    GetFlushLog(Box<Future<Item = Vec<Log>, Error = ()>>),
    GetFlushReceipt(Box<Log>, Box<Future<Item = Option<TransactionReceipt>, Error = ()>>),
}

/// Future for checking if the flush block is set in relay, then getting the transaction receipt if so
pub struct CheckForPastFlush<T: DuplexTransport + 'static> {
    state: CheckForPastFlushState,
    source: Network<T>,
    tx: mpsc::UnboundedSender<(Log, TransactionReceipt)>,
}

impl<T: DuplexTransport + 'static> CheckForPastFlush<T> {
    /// Create a new CheckForPastFlush Future
    /// # Arguments
    ///
    /// * `source` - Network being flushed
    /// * `tx` - Sender to trigger Flush event processor
    pub fn new(source: &Network<T>, tx: &mpsc::UnboundedSender<(Log, TransactionReceipt)>) -> Self {
        let flush_block_query = FlushBlockQuery::new();
        let flush_block_future = source
            .relay
            .query::<FlushBlock, Address, BlockNumber, FlushBlockQuery>(
                "flushBlock",
                flush_block_query,
                source.account,
                Options::default(),
                BlockNumber::Latest,
            )
            .map_err(|e| {
                error!("error retrieving flush block: {:?}", e);
            });
        CheckForPastFlush {
            state: CheckForPastFlushState::CheckFlushBlock(Box::new(flush_block_future)),
            source: source.clone(),
            tx: tx.clone(),
        }
    }
    /// Create a Future to get the flush even logs given the block the event occurred at
    /// # Arguments
    ///
    /// * `block` - Block number for the flush event
    pub fn get_flush_log(&self, block_number: u64) -> Box<Future<Item = Vec<Log>, Error = ()>> {
        // Not sure if there is a better way to get the log we want, probably get block or something
        let filter = FilterBuilder::default()
            .address(vec![self.source.relay.address()])
            .from_block(BlockNumber::from(block_number - 1))
            .to_block(BlockNumber::Number(block_number + 1))
            .topics(Some(vec![FLUSH_EVENT_SIGNATURE.into()]), None, None, None)
            .build();
        Box::new(self.source.web3.eth().logs(filter).map_err(|e| {
            error!("error getting flush log: {:?}", e);
        }))
    }
}

impl<T: DuplexTransport + 'static> Future for CheckForPastFlush<T> {
    type Item = ();
    type Error = ();
    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        loop {
            let source = self.source.clone();
            let tx = self.tx.clone();
            let next = match self.state {
                CheckForPastFlushState::CheckFlushBlock(ref mut future) => {
                    let block = try_ready!(future.poll());
                    let block_number = block.0.as_u64();
                    info!("flush block on start is {}", block_number);
                    if block_number > 0 {
                        let future = self.get_flush_log(block_number);
                        CheckForPastFlushState::GetFlushLog(future)
                    } else {
                        return Ok(Async::Ready(()));
                    }
                }
                CheckForPastFlushState::GetFlushLog(ref mut stream) => {
                    let logs = try_ready!(stream.poll());
                    if !logs.is_empty() {
                        let log = &logs[0];
                        let removed = log.removed.unwrap_or(false);
                        match log.transaction_hash {
                            Some(tx_hash) => {
                                let future = source.get_receipt(removed, tx_hash);
                                CheckForPastFlushState::GetFlushReceipt(Box::new(log.clone()), Box::new(future))
                            }
                            None => {
                                error!("flush log missing transaction hash");
                                return Err(());
                            }
                        }
                    } else {
                        error!("Didn't find any flush event at flushBlock");
                        return Ok(Async::Ready(()));
                    }
                }
                CheckForPastFlushState::GetFlushReceipt(ref log, ref mut future) => {
                    let receipt_option = try_ready!(future.poll());
                    match receipt_option {
                        Some(receipt) => {
                            let send_result = tx.unbounded_send((*log.clone(), receipt));
                            if send_result.is_err() {
                                error!("error sending log & receipt");
                                return Err(());
                            }
                            return Ok(Async::Ready(()));
                        }
                        None => {
                            error!("error getting flush receipt");
                            return Err(());
                        }
                    }
                }
            };
            self.state = next;
        }
    }
}

enum ProcessFlushState<T: DuplexTransport + 'static> {
    Wait,
    CheckBalances(CheckBalances<T>),
    FilterContracts(FilterContracts),
    FilterLowBalance(FilterLowBalance),
    WithdrawWallets(Box<Future<Item = Vec<()>, Error = ()>>),
    WithdrawLeftovers(WithdrawLeftovers<T>),
}

/// Process Flush Log/Receipts that come across and trigger a multi step flush
/// 1. Check all balances
/// 1. Filter contracts out
/// 1. Filter out balances that don't have more than the fee cost to withdraw
/// 1. Withdraw all balances to the same wallet on target chain
/// 1. Withdraw any leftovers in the contract after all withdrawals confirmed
pub struct ProcessFlush<T: DuplexTransport + 'static> {
    state: ProcessFlushState<T>,
    source: Network<T>,
    target: Network<T>,
    rx: mpsc::UnboundedReceiver<(Log, TransactionReceipt)>,
    flush_receipt: Option<TransactionReceipt>,
}

impl<T: DuplexTransport + 'static> ProcessFlush<T> {
    /// Create a new ProcessFlush Future
    /// # Arguments
    ///
    /// * `source` - Network being flushed
    /// * `target` - Network to send all the token balances
    /// * `rx` - Receiver that triggers on Flush event
    pub fn new(
        source: &Network<T>,
        target: &Network<T>,
        rx: mpsc::UnboundedReceiver<(Log, TransactionReceipt)>,
    ) -> Self {
        ProcessFlush {
            state: ProcessFlushState::Wait,
            source: source.clone(),
            target: target.clone(),
            rx,
            flush_receipt: None,
        }
    }
}

impl<T: DuplexTransport + 'static> Future for ProcessFlush<T> {
    type Item = ();
    type Error = ();
    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        let target = self.target.clone();
        let source = self.source.clone();
        loop {
            let flush_receipt = self.flush_receipt.clone();
            let next: ProcessFlushState<T> = match self.state {
                ProcessFlushState::Wait => {
                    let event = try_ready!(self.rx.poll());
                    match event {
                        Some((log, receipt)) => {
                            info!("flush event triggered");
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
                    info!("found {} wallets with tokens", balances.len());
                    let contract_future = FilterContracts::new(&source, balances);
                    ProcessFlushState::FilterContracts(contract_future)
                }
                ProcessFlushState::FilterContracts(ref mut future) => {
                    let balances = try_ready!(future.poll());
                    info!("{} balances were not contracts", balances.len());
                    let low_balance_future = FilterLowBalance::new(&target, balances);
                    ProcessFlushState::FilterLowBalance(low_balance_future)
                }
                ProcessFlushState::FilterLowBalance(ref mut future) => {
                    let balances = try_ready!(future.poll());
                    info!("{} wallets above minimum balances", balances.len());
                    match flush_receipt {
                        Some(receipt) => {
                            let futures: Vec<Box<Future<Item = (), Error = ()>>> = balances
                                .iter()
                                .filter_map(|(address, balance)| {
                                    let transfer = Transfer::from_receipt(*address, *balance, false, &receipt);
                                    match transfer {
                                        Ok(t) => Some(t.approve_withdrawal(&source, &target, false)),
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
                    info!("finished all wallet withdrawals");
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
                    info!("finished leftover withdrawal");
                    return Ok(Async::Ready(()));
                }
            };
            self.state = next;
        }
    }
}

/// Fee Wallet struct for taking the FeeWalletQuery and parsing the Tokens
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

/// Query with args for getting the fee wallet
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

/// Future that gets the balance of the relay contract on target chain.
/// Then, it withdraws that balance to the fee wallet
pub struct WithdrawLeftovers<T: DuplexTransport + 'static> {
    state: WithdrawLeftoversState,
    source: Network<T>,
    target: Network<T>,
    receipt: TransactionReceipt,
    balance: Option<U256>,
}

impl<T: DuplexTransport + 'static> WithdrawLeftovers<T> {
    /// Create a new WithdrawLeftovers Future
    /// # Arguments
    ///
    /// * `source` - Network being flushed
    /// * `target` - Network to send all the token balances
    /// * `flush_receipt` - Transaction receipt for the flush event
    fn new(source: &Network<T>, target: &Network<T>, flush_receipt: &TransactionReceipt) -> Self {
        let state = WithdrawLeftoversState::GetBalance(WithdrawLeftovers::get_balance(&target));
        WithdrawLeftovers {
            state,
            source: source.clone(),
            target: target.clone(),
            receipt: flush_receipt.clone(),
            balance: None,
        }
    }

    /// Get a future to find the balance of the relay address
    /// # Arguments
    ///
    /// * `target` - Network being flushed
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
                }),
        )
    }

    /// Get a future to find the fee wallet for the target chain
    /// # Arguments
    ///
    /// * `target` - Network being flushed
    fn get_fee_wallet(target: &Network<T>) -> Box<Future<Item = FeeWallet, Error = ()>> {
        let fee_wallet_query = FeeWalletQuery::new();
        let target = target.clone();
        Box::new(
            target
                .relay
                .query::<FeeWallet, Address, BlockNumber, FeeWalletQuery>(
                    "feeWallet",
                    fee_wallet_query,
                    target.account,
                    Options::default(),
                    BlockNumber::Latest,
                )
                .map_err(|e| {
                    error!("error retrieving fee wallet: {:?}", e);
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
                        Some(b) => {
                            info!("withdrawing {} to fee wallet {}", b, address.0);
                            match Transfer::from_receipt(address.0, b, false, &receipt) {
                                Ok(transfer) => WithdrawLeftoversState::Withdraw(
                                    transfer.approve_withdrawal(&source, &target, false),
                                ),
                                Err(e) => {
                                    error!("Error creating transaction from flush receipt: {:?}", e);
                                    return Err(());
                                }
                            }
                        }
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

/// Fees struct for taking the FeeQuery and parsing the Tokens
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

/// Query with args for getting the fees
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

/// FilterLowBalance Future that takes a list of wallets, and filters out that have a balance below the fee cost to withdraw
pub struct FilterLowBalance {
    future: Box<Future<Item = Fees, Error = ()>>,
    wallets: Vec<(Address, U256)>,
}

impl FilterLowBalance {
    /// Create a new FilterContracts Future
    /// # Arguments
    ///
    /// * `source` - Network where the wallets may be contracts
    /// * `wallets` - Vector of tuples with Address and Balances
    pub fn new<T: DuplexTransport + 'static>(target: &Network<T>, wallets: Vec<(Address, U256)>) -> Self {
        // Get contract data
        let future = target
            .relay
            .query::<Fees, Address, BlockNumber, FeeQuery>(
                "fees",
                FeeQuery::new(),
                target.account,
                Options::default(),
                BlockNumber::Latest,
            )
            .map_err(|e| {
                error!("error getting relay fees: {:?}", e);
            });

        FilterLowBalance {
            future: Box::new(future),
            wallets,
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
            .wallets
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

/// FilterContract Future that takes a list of wallets, and filters out any that are contracts
pub struct FilterContracts {
    future: Box<Future<Item = Vec<Bytes>, Error = ()>>,
    wallets: Vec<(Address, U256)>,
}

impl FilterContracts {
    /// Create a new FilterContracts Future
    /// # Arguments
    ///
    /// * `source` - Network where the wallets may be contracts
    /// * `wallets` - Vector of tuples with Address and Balances
    pub fn new<T: DuplexTransport + 'static>(source: &Network<T>, wallets: Vec<(Address, U256)>) -> Self {
        // Get contract data
        // For whatever reason, doing this as an iter().map().collect() did not work
        let mut futures = Vec::new();
        for (address, _balance) in wallets.clone() {
            let future = source.web3.eth().code(address, None).map_err(move |e| {
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
                if bytes.0.is_empty() {
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

/// Get all balances for all wallets with tokens by looking over the entire history of the chain.
/// Keep track of all balances, and add and remove as transfers occur
/// In order to avoid crashes, it does this by looking at windows of 1000 blocks rather than the whole chain
pub struct CheckBalances<T: DuplexTransport + 'static> {
    source: Network<T>,
    state: CheckBalancesState,
    balances: HashMap<Address, U256>,
}

impl<T: DuplexTransport + 'static> CheckBalances<T> {
    /// Create a new CheckBalance Future
    /// # Arguments
    ///
    /// * `source` - Network where the transfers were performed
    /// * `block` - Optional ending block, gets latest if None
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

    /// Build a filter and look at all logs in the given range for transfer events.
    /// # Arguments
    ///
    /// * `source` - Network where the transfers were performed
    /// * `start` - Start of the next window
    /// * `end` - End of the next window
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
