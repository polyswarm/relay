use web3::contract::Options;
use web3::futures::future::join_all;
use web3::futures::future::{ok, Either};
use web3::futures::prelude::*;
use web3::futures::sync::mpsc;
use web3::futures::try_ready;
use web3::types::{Address, BlockNumber, Bytes, TransactionReceipt, U256};
use web3::DuplexTransport;

use crate::eth::transaction::SendTransaction;
use crate::flush::{CheckBalances, FilterLowBalance, Wallet};
use crate::relay::Network;
use crate::eth::Event;
use crate::transfers::withdrawal::{ApproveParams, WaitForWithdrawalProcessed};

enum ProcessFlushState<T: DuplexTransport + 'static> {
    Wait,
    CheckBalances(CheckBalances<T>),
    FilterContracts(FilterContracts),
    FilterLowBalance(FilterLowBalance),
    WithdrawWallets(Box<dyn Future<Item = Vec<()>, Error = ()>>),
    WithdrawLeftovers(FlushRemaining<T>),
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
    rx: mpsc::UnboundedReceiver<Event>,
    flush: Option<Event>,
}

impl<T: DuplexTransport + 'static> ProcessFlush<T> {
    /// Create a new ProcessFlush Future
    /// # Arguments
    ///
    /// * `source` - Network being flushed
    /// * `target` - Network to send all the token balances
    /// * `rx` - Receiver that triggers on Flush event
    pub fn new(source: &Network<T>, target: &Network<T>, rx: mpsc::UnboundedReceiver<Event>) -> Self {
        ProcessFlush {
            state: ProcessFlushState::Wait,
            source: source.clone(),
            target: target.clone(),
            rx,
            flush: None,
        }
    }

    fn handle_flush_event(&self, flush: Event) -> Result<Option<CheckBalances<T>>, ()> {
        if let Ok(mut lock) = self.source.flushed.write() {
            *lock = Some(flush.clone());
        } else {
            error!("error getting write lock on process");
            return Err(());
        }

        info!("flush event triggered");
        let removed = flush.log.removed.unwrap_or(false);
        if removed {
            Ok(None)
        } else {
            if flush.receipt.block_hash.is_none() {
                error!("Failed to get block hash for flush");
                return Err(());
            }

            if flush.receipt.block_number.is_none() {
                error!("Failed to get block hash for flush");
                return Err(());
            }

            Ok(Some(CheckBalances::new(&self.source, flush.receipt.block_number)))
        }
    }

    fn handle_final_wallets(
        &self,
        flush_event: &Event,
        fees: &U256,
        wallets: &mut Vec<Wallet>,
    ) -> Result<impl Future<Item = Vec<()>, Error = ()>, ()> {
        let receipt = flush_event.receipt.clone();
        if receipt.block_number.is_none() {
            error!("no block number in transfer receipt");
            return Err(());
        }

        if receipt.block_hash.is_none() {
            error!("no block hash in transfer receipt");
            return Err(());
        }

        let block_hash = receipt.block_hash.unwrap();
        let block_number = receipt.block_number.unwrap();
        let fees = *fees;
        wallets.sort();

        let futures: Vec<Box<dyn Future<Item = (), Error = ()>>> = wallets
            .iter()
            .enumerate()
            .map(|(i, wallet)| {
                let target = self.target.clone();
                let wait_target = self.target.clone();
                let transaction_hash = receipt.transaction_hash;
                let wallet = *wallet;

                // Create futures
                let transfer = wallet.get_transfer(&receipt.transaction_hash, &block_hash, block_number + i);
                let future: Box<dyn Future<Item = (), Error = ()>> = Box::new(
                    transfer
                        .check_withdrawal(&target, Some(fees))
                        .and_then(move |needs_approval| {
                            let target = target.clone();
                            if needs_approval {
                                Either::A(wallet.withdraw(&target, &transaction_hash, &block_hash, block_number + i))
                            } else {
                                Either::B(ok(()))
                            }
                        })
                        .and_then(move |_| {
                            let target = wait_target.clone();
                            // Wait until it is processed before moving on
                            WaitForWithdrawalProcessed::new(&target, &transfer)
                        }),
                );
                future
            })
            .collect();
        Ok(join_all(futures))
    }
}

impl<T: DuplexTransport + 'static> Future for ProcessFlush<T> {
    type Item = ();
    type Error = ();
    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        let target = self.target.clone();
        let source = self.source.clone();
        loop {
            let flush = self.flush.clone();
            let next: ProcessFlushState<T> = match self.state {
                ProcessFlushState::Wait => {
                    let event = try_ready!(self.rx.poll());
                    match event {
                        Some(flush) => {
                            self.flush = Some(flush.clone());
                            if let Some(future) = self.handle_flush_event(flush)? {
                                ProcessFlushState::CheckBalances(future)
                            } else {
                                ProcessFlushState::Wait
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
                    let (fees, mut balances) = try_ready!(future.poll());
                    info!("{} wallets above minimum balances", balances.len());
                    match flush {
                        Some(flush_event) => {
                            let futures = self.handle_final_wallets(&flush_event, &fees, &mut balances)?;
                            ProcessFlushState::WithdrawWallets(Box::new(futures))
                        }
                        None => {
                            error!("No flush receipt available");
                            return Err(());
                        }
                    }
                }
                ProcessFlushState::WithdrawWallets(ref mut future) => {
                    let withdrawals = try_ready!(future.poll());
                    info!("finished {} wallet withdrawals", withdrawals.len());
                    if let Some(flush_event) = flush {
                        ProcessFlushState::WithdrawLeftovers(FlushRemaining::new(
                            &target,
                            &flush_event.receipt,
                            withdrawals.len() + 1,
                        ))
                    } else {
                        error!("No flush receipt available");
                        return Err(());
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

enum FlushRemainingState<T: DuplexTransport + 'static> {
    GetBalance(Box<dyn Future<Item = U256, Error = ()>>),
    GetFeeWallet(Box<dyn Future<Item = Address, Error = ()>>),
    Withdraw(SendTransaction<T, ApproveParams>),
}

/// Future that gets the balance of the relay contract on target chain.
/// Then, it withdraws that balance to the fee wallet
pub struct FlushRemaining<T: DuplexTransport + 'static> {
    state: FlushRemainingState<T>,
    target: Network<T>,
    receipt: TransactionReceipt,
    offset: usize,
    balance: Option<U256>,
}

impl<T: DuplexTransport + 'static> FlushRemaining<T> {
    /// Create a new WithdrawLeftovers Future
    /// # Arguments
    ///
    /// * `source` - Network being flushed
    /// * `target` - Network to send all the token balances
    /// * `flush_receipt` - Transaction receipt for the flush event
    fn new(target: &Network<T>, flush_receipt: &TransactionReceipt, offset: usize) -> Self {
        let state = FlushRemainingState::GetBalance(FlushRemaining::get_balance(&target));
        FlushRemaining {
            state,
            target: target.clone(),
            receipt: flush_receipt.clone(),
            offset,
            balance: None,
        }
    }

    /// Get a future to find the balance of the relay address
    /// # Arguments
    ///
    /// * `target` - Network being flushed
    fn get_balance(target: &Network<T>) -> Box<dyn Future<Item = U256, Error = ()>> {
        let target = target.clone();
        Box::new(
            target
                .token
                .query(
                    "balanceOf",
                    target.relay.address(),
                    None,
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
    fn get_fee_wallet(target: &Network<T>) -> Box<dyn Future<Item = Address, Error = ()>> {
        let target = target.clone();
        Box::new(
            target
                .relay
                .query("feeWallet", (), None, Options::default(), BlockNumber::Latest)
                .map_err(|e| {
                    error!("error retrieving fee wallet: {:?}", e);
                }),
        )
    }
}

impl<T: DuplexTransport + 'static> Future for FlushRemaining<T> {
    type Item = ();
    type Error = ();
    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        loop {
            let target = self.target.clone();
            let receipt = self.receipt.clone();
            let offset = self.offset;
            let next = match self.state {
                FlushRemainingState::GetBalance(ref mut future) => {
                    let balance = try_ready!(future.poll());
                    if balance == U256::zero() {
                        info!("contract balance is zero, no remaining NCT to withdraw");
                        return Ok(Async::Ready(()));
                    }
                    self.balance = Some(balance);
                    FlushRemainingState::GetFeeWallet(FlushRemaining::get_fee_wallet(&target))
                }
                FlushRemainingState::GetFeeWallet(ref mut future) => {
                    let address = try_ready!(future.poll());
                    match self.balance {
                        Some(b) => {
                            if receipt.block_number.is_none() {
                                error!("no block number in transfer receipt");
                                return Err(());
                            }

                            if receipt.block_hash.is_none() {
                                error!("no block hash in transfer receipt");
                                return Err(());
                            }
                            let block_hash = receipt.block_hash.unwrap();
                            let block_number = receipt.block_number.unwrap() + offset;
                            let wallet = Wallet::new(&address, &b);
                            info!("withdrawing {} to fee wallet {}", wallet.balance, wallet.address);
                            let withdrawal =
                                wallet.withdraw(&target, &receipt.transaction_hash, &block_hash, block_number);
                            FlushRemainingState::Withdraw(withdrawal)
                        }
                        None => {
                            error!("error balance is none");
                            return Err(());
                        }
                    }
                }
                FlushRemainingState::Withdraw(ref mut future) => {
                    try_ready!(future.poll());
                    return Ok(Async::Ready(()));
                }
            };
            self.state = next;
        }
    }
}

/// FilterContract Future that takes a list of wallets, and filters out any that are contracts
/// Also removes any zero address wallets
pub struct FilterContracts {
    future: Box<dyn Future<Item = Vec<Bytes>, Error = ()>>,
    wallets: Vec<Wallet>,
}

impl FilterContracts {
    /// Create a new FilterContracts Future
    /// # Arguments
    ///
    /// * `source` - Network where the wallets may be contracts
    /// * `wallets` - Vector of tuples with Address and Balances
    pub fn new<T: DuplexTransport + 'static>(source: &Network<T>, wallets: Vec<Wallet>) -> Self {
        // Get contract data
        // For whatever reason, doing this as an iter().map().collect() did not work
        let wallets: Vec<Wallet> = wallets
            .iter()
            .filter(|wallet| wallet.address != Address::zero())
            .cloned()
            .collect();
        let mut futures = Vec::new();
        let source = source.clone();
        for wallet in wallets.clone() {
            futures.push(Box::new(source.web3.eth().code(wallet.address, None).map_err(
                move |e| {
                    error!("error retrieving code for wallet {}: {:?}", wallet.address, e);
                },
            )));
        }

        FilterContracts {
            future: Box::new(join_all(futures)),
            wallets,
        }
    }
}

impl Future for FilterContracts {
    type Item = Vec<Wallet>;
    type Error = ();
    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        let all_bytes = try_ready!(self.future.poll());
        let filtered = self
            .wallets
            .iter()
            .zip(all_bytes)
            .filter_map(
                move |(wallet, ref bytes)| {
                    if bytes.0.is_empty() {
                        Some(*wallet)
                    } else {
                        None
                    }
                },
            )
            .collect();
        Ok(Async::Ready(filtered))
    }
}
