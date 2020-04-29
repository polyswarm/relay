use std::cmp;
use std::collections::HashMap;
use web3::contract::Options;
use web3::futures::prelude::*;
use web3::futures::try_ready;
use web3::types::{Address, BlockNumber, FilterBuilder, Log, H256, U256, U64};

use crate::eth::contracts::TRANSFER_EVENT_SIGNATURE;
use crate::eth::transaction::SendTransaction;
use crate::relay::network::Network;
use crate::events::transfers::transfer::Transfer;
use crate::events::transfers::withdrawal::ApproveParams;
use web3::DuplexTransport;

pub enum CheckBalancesState {
    GetEndingBlock(Box<dyn Future<Item = U64, Error = ()>>),
    GetLogWindow(u64, u64, Box<dyn Future<Item = Vec<Log>, Error = ()>>),
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
    pub fn new(source: &Network<T>, block: Option<U64>) -> Self {
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
    fn build_next_window(source: &Network<T>, start: u64, end: u64) -> Box<dyn Future<Item = Vec<Log>, Error = ()>> {
        let token_address: Address = source.token.address();
        let filter = FilterBuilder::default()
            .address(vec![token_address])
            .from_block(BlockNumber::from(start))
            .to_block(BlockNumber::from(end))
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
    type Item = Vec<Wallet>;
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
                    debug!(
                        "found {} logs with transfers between {} and {}",
                        logs.len(),
                        window_end - 1000,
                        window_end,
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
                            self.balances
                                .iter()
                                .map(|(key, value)| Wallet::new(key, value))
                                .collect(),
                        ));
                    }
                }
            };
            self.state = next;
        }
    }
}

/// FilterLowBalance Future that takes a list of wallets, and filters out that have a balance below the fee cost to withdraw
pub struct FilterLowBalance {
    future: Box<dyn Future<Item = U256, Error = ()>>,
    wallets: Vec<Wallet>,
}

impl FilterLowBalance {
    /// Create a new FilterContracts Future
    /// # Arguments
    ///
    /// * `source` - Network where the wallets may be contracts
    /// * `wallets` - Vector of tuples with Address and Balances
    pub fn new<T: DuplexTransport + 'static>(target: &Network<T>, wallets: Vec<Wallet>) -> Self {
        // Get contract data
        let future = target
            .relay
            .query("fees", (), None, Options::default(), BlockNumber::Latest)
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
    type Item = (U256, Vec<Wallet>);
    type Error = ();
    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        let fees = try_ready!(self.future.poll());
        let filtered = self
            .wallets
            .iter()
            .filter_map(|wallet| if wallet.balance > fees { Some(*wallet) } else { None })
            .collect();
        Ok(Async::Ready((fees, filtered)))
    }
}

/// Simple struct with an Address and Balance that represents an ethereum wallet
#[derive(Clone, Copy, Ord, Eq, PartialEq, PartialOrd)]
pub struct Wallet {
    pub address: Address,
    pub balance: U256,
}

impl Wallet {
    /// Create a new wallet
    /// # Arguments
    ///
    /// * `address` - Address for the wallet
    /// * `balance` - Current balance of tokens at the wallet
    pub fn new(address: &Address, balance: &U256) -> Self {
        Wallet {
            address: *address,
            balance: *balance,
        }
    }

    /// Creates a transfer for this wallet given the flush tx hash, block hash, and the block number with offset
    /// # Arguments
    ///
    /// * `transaction_hash`- Transaction hash of the flush
    /// * `block_hash` - Block hash of the flush
    /// * `block_number` - Modified block number of the flush. Will not match the actual block number to avoid collisions in the contract
    pub fn get_transfer(&self, transaction_hash: &H256, block_hash: &H256, block_number: U64) -> Transfer {
        Transfer {
            destination: self.address,
            amount: self.balance,
            tx_hash: *transaction_hash,
            block_hash: *block_hash,
            block_number,
            removed: false,
        }
    }

    /// Create a SendTransaction Future that performs a withdrawal on the target chain
    /// # Arguments
    ///
    /// * `target` - Network to withdrawl from
    /// * `transaction_hash`- Transaction hash of the flush
    /// * `block_hash` - Block hash of the flush
    /// * `block_number` - Modified block number of the flush. Will not match the actual block number to avoid collisions in the contract
    pub fn withdraw<T: DuplexTransport + 'static>(
        &self,
        target: &Network<T>,
        transaction_hash: &H256,
        block_hash: &H256,
        block_number: U64,
    ) -> SendTransaction<T, ApproveParams> {
        let approve_params = ApproveParams {
            destination: self.address,
            amount: self.balance,
            tx_hash: *transaction_hash,
            block_hash: *block_hash,
            block_number,
        };
        SendTransaction::new(&target, "approveWithdrawal", &approve_params, target.retries)
    }
}
