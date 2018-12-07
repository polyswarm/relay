use std::fmt;
use std::rc::Rc;
use std::time;
use tokio_core::reactor;
use web3::confirm::wait_for_transaction_confirmation;
use web3::contract::Options;
use web3::futures::prelude::*;
use web3::futures::sync::mpsc;
use web3::types::{Address, FilterBuilder, H256, U256};
use web3::DuplexTransport;

use super::contracts::TRANSFER_EVENT_SIGNATURE;
use super::relay::Network;
use super::withdrawal::GetWithdrawal;

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
    pub fn get_withdrawal<T: DuplexTransport + 'static>(&self, target: &Rc<Network<T>>) -> GetWithdrawal {
        GetWithdrawal::new(target, *self)
    }

    pub fn approve_withdrawal<T: DuplexTransport + 'static>(&self, target: &Rc<Network<T>>) -> ApproveWithdrawal {
        ApproveWithdrawal::new(target, *self)
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

/// Future that calls the ERC20RelayContract to approve a transfer across the relay
pub struct ApproveWithdrawal(Box<Future<Item = (), Error = ()>>);

impl ApproveWithdrawal {
    pub fn new<T: DuplexTransport + 'static>(network: &Rc<Network<T>>, transfer: Transfer) -> Self {
        let network = network.clone();
        let future = network
            .clone()
            .relay
            .call_with_confirmations(
                "approveWithdrawal",
                (
                    transfer.destination,
                    transfer.amount,
                    transfer.tx_hash,
                    transfer.block_hash,
                    transfer.block_number,
                ),
                network.account,
                Options::with(|options| {
                    options.gas = Some(network.get_gas_limit());
                    options.gas_price = Some(network.get_gas_price());
                }),
                network.confirmations as usize,
            ).and_then(|receipt| {
                info!("withdrawal approved: {:?}", receipt);
                Ok(())
            }).map_err(|e| {
                error!("error approving withdrawal: {:?}", e);
                ()
            });
        ApproveWithdrawal(Box::new(future))
    }
}

impl Future for ApproveWithdrawal {
    type Item = ();
    type Error = ();
    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        self.0.poll()
    }
}

/// Future to start up and process a stream of transfers
pub struct HandleTransfers(Box<Future<Item = (), Error = ()>>);

impl HandleTransfers {
    pub fn new<T: DuplexTransport + 'static>(
        source: &Network<T>,
        target: &Rc<Network<T>>,
        handle: &reactor::Handle,
    ) -> Self {
        let handle = handle.clone();
        let target = target.clone();
        let future = WatchTransfers::new(source, &handle).for_each(move |transfer: Transfer| {
            let target = target.clone();
            handle.spawn(transfer.approve_withdrawal(&target));
            Ok(())
        });
        HandleTransfers(Box::new(future))
    }
}

impl Future for HandleTransfers {
    type Item = ();
    type Error = ();
    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        self.0.poll()
    }
}

/// Stream of transfer events that have been on the main chain for N blocks.
/// N is confirmations per settings.
struct WatchTransfers(mpsc::UnboundedReceiver<Transfer>);

impl WatchTransfers {
    fn new<T: DuplexTransport + 'static>(source: &Network<T>, handle: &reactor::Handle) -> Self {
        let (tx, rx) = mpsc::unbounded();
        let filter = FilterBuilder::default()
            .address(vec![source.token.address()])
            .topics(
                Some(vec![TRANSFER_EVENT_SIGNATURE.into()]),
                None,
                Some(vec![source.relay.address().into()]),
                None,
            ).build();

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
                                    ).and_then(move |receipt| {
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
                                    }).or_else(|e| {
                                        error!("error waiting for transfer confirmations: {}", e);
                                        Ok(())
                                    }),
                                );

                                Ok(())
                            },
                        )
                    })
                }).or_else(move |e| {
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
