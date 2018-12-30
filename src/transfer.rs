use ethabi::Token;
use std::fmt;
use std::rc::Rc;
use std::time;
use tokio_core::reactor;
use web3::confirm::wait_for_transaction_confirmation;
use web3::contract::tokens::Tokenize;
use web3::futures::prelude::*;
use web3::futures::sync::mpsc;
use web3::futures::try_ready;
use web3::types::{Address, FilterBuilder, H256, U256};
use web3::DuplexTransport;

use super::contracts::TRANSFER_EVENT_SIGNATURE;
use super::relay::Network;
use super::transaction::SendTransaction;
use super::withdrawal::DoesRequireApproval;

/// Represents a token transfer between two networks
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct Transfer {
    pub destination: Address,
    pub amount: U256,
    pub tx_hash: H256,
    pub block_hash: H256,
    pub block_number: U256,
}

impl Transfer {
    /// Returns a Future that will get the Withdrawal from the contract
    ///
    /// # Arguments
    ///
    /// * `target` - Network that withdrawals are posted to
    pub fn check_withdrawal<T: DuplexTransport + 'static>(&self, target: &Rc<Network<T>>) -> DoesRequireApproval<T> {
        DoesRequireApproval::new(target, self)
    }

    /// Returns a Future that will transaction with "approve_withdrawal" on the ERC20Relay contract
    ///
    /// # Arguments
    ///
    /// * `target` - Network where the withdrawals is performed
    pub fn approve_withdrawal<T: DuplexTransport + 'static>(
        &self,
        target: &Rc<Network<T>>,
    ) -> SendTransaction<T, Self> {
        info!("approving withdrawal {}", self);
        SendTransaction::new(target, "approveWithdrawal", self)
    }
}

impl Tokenize for Transfer {
    fn into_tokens(self) -> Vec<Token> {
        let mut tokens = Vec::new();
        tokens.push(Token::Address(self.destination));
        tokens.push(Token::Uint(self.amount));
        tokens.push(Token::FixedBytes(self.tx_hash.to_vec()));
        tokens.push(Token::FixedBytes(self.block_hash.to_vec()));
        tokens.push(Token::Uint(self.block_number));
        tokens
    }
}

impl fmt::Display for Transfer {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "({} -> {:?}, hash: {:?})",
            self.amount, self.destination, self.tx_hash
        )
    }
}

/// Future to start up and process a stream of transfers
pub struct HandleTransfers<T: DuplexTransport + 'static> {
    target: Rc<Network<T>>,
    stream: WatchTransfers,
    handle: reactor::Handle,
}

impl<T: DuplexTransport + 'static> HandleTransfers<T> {
    /// Returns a newly created HandleTransfers Future
    ///
    /// # Arguments
    ///
    /// * `source` - Network where the transfers are performed
    /// * `target` - Network where the withdrawal will be posted to the contract
    /// * `handle` - Handle to spawn new futures
    pub fn new(source: &Network<T>, target: &Rc<Network<T>>, handle: &reactor::Handle) -> Self {
        let handle = handle.clone();
        let target = target.clone();
        let stream = WatchTransfers::new(source, &handle);
        HandleTransfers { target, stream, handle }
    }
}

impl<T: DuplexTransport + 'static> Future for HandleTransfers<T> {
    type Item = ();
    type Error = ();
    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        loop {
            let transfer = try_ready!(self.stream.poll());
            if let Some(t) = transfer {
                self.handle.spawn(t.approve_withdrawal(&self.target));
            }
        }
    }
}

/// Stream of transfer events that have been on the main chain for N blocks.
/// N is confirmations per settings.
struct WatchTransfers(mpsc::UnboundedReceiver<Transfer>);

impl WatchTransfers {
    /// Returns a newly created WatchTransfers Stream
    ///
    /// # Arguments
    ///
    /// * `source` - Network where the transfers are performed
    /// * `handle` - Handle to spawn new futures
    fn new<T: DuplexTransport + 'static>(source: &Network<T>, handle: &reactor::Handle) -> Self {
        let (tx, rx) = mpsc::unbounded();
        let filter = FilterBuilder::default()
            .address(vec![source.token.address()])
            .topics(
                Some(vec![TRANSFER_EVENT_SIGNATURE.into()]),
                None,
                Some(vec![source.relay.address().into()]),
                None,
            )
            .build();

        let future = {
            let network_type = source.network_type;
            let confirmations = source.confirmations as usize;
            let handle = handle.clone();
            let transport = source.web3.transport().clone();
            source
                .web3
                .eth_subscribe()
                .subscribe_logs(filter)
                .and_then(move |sub| {
                    sub.for_each(move |log| {
                        if Some(true) == log.removed {
                            warn!("received removed log, revoke votes");
                            return Ok(());
                        }

                        log.transaction_hash.map_or_else(
                            || {
                                warn!("log missing transaction hash");
                                Ok(())
                            },
                            |tx_hash| {
                                let tx = tx.clone();
                                let destination: Address = log.topics[1].into();
                                let amount: U256 = log.data.0[..32].into();

                                info!(
                                    "received transfer event in tx hash {:?}, waiting for confirmations",
                                    &tx_hash
                                );

                                handle.spawn(
                                    wait_for_transaction_confirmation(
                                        transport.clone(),
                                        tx_hash,
                                        time::Duration::from_secs(1),
                                        confirmations,
                                    )
                                    .and_then(move |receipt| {
                                        if receipt.block_number.is_none() {
                                            warn!("no block number in transfer receipt");
                                            return Ok(());
                                        }

                                        if receipt.block_hash.is_none() {
                                            warn!("no block hash in transfer receipt");
                                            return Ok(());
                                        }

                                        let block_hash = receipt.block_hash.unwrap();
                                        let block_number = receipt.block_number.unwrap();

                                        let transfer = Transfer {
                                            destination,
                                            amount,
                                            tx_hash,
                                            block_hash,
                                            block_number,
                                        };

                                        info!("transfer event confirmed, approving {}", &transfer);
                                        tx.unbounded_send(transfer).unwrap();
                                        Ok(())
                                    })
                                    .or_else(|e| {
                                        error!("error waiting for transfer confirmations: {}", e);
                                        Ok(())
                                    }),
                                );

                                Ok(())
                            },
                        )
                    })
                })
                .or_else(move |e| {
                    error!("error in {:?} transfer stream: {}", network_type, e);
                    Ok(())
                })
        };

        handle.spawn(future);

        WatchTransfers(rx)
    }
}

impl Stream for WatchTransfers {
    type Item = Transfer;
    type Error = ();
    fn poll(&mut self) -> Poll<Option<Self::Item>, Self::Error> {
        self.0.poll()
    }
}
