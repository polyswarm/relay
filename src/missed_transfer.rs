use std::rc::Rc;
use tokio_core::reactor;
use web3::futures::prelude::*;
use web3::futures::sync::mpsc;
use web3::types::{Address, BlockNumber, FilterBuilder, U256};
use web3::DuplexTransport;

use super::contracts::TRANSFER_EVENT_SIGNATURE;
use super::relay::Network;
use super::transfer::Transfer;

pub const LOOKBACK_RANGE: u64 = 100;
pub const LOOKBACK_LEEWAY: u64 = 5;

/// Stream of Transfer that were missed, either from downtime, or failed approvals
pub struct FindMissedTransfers(mpsc::UnboundedReceiver<Transfer>);

impl FindMissedTransfers {
    /// Returns a newly created FindMissedTransfers Stream
    ///
    /// # Arguments
    ///
    /// * `source` - Network where the missed transfers are found
    /// * `handle` - Handle to spawn new futures
    pub fn new<T: DuplexTransport + 'static>(source: &Network<T>, handle: &reactor::Handle) -> Self {
        let (tx, rx) = mpsc::unbounded();
        let h = handle.clone();
        let token_address = source.token.address();
        let relay_address = source.relay.address();
        let network_type = source.network_type;
        let interval = source.interval;
        let confirmations = source.confirmations;
        let web3 = source.web3.clone();
        let future = web3
            .clone()
            .eth_subscribe()
            .subscribe_new_heads()
            .and_then(move |sub| {
                let handle = h.clone();
                let tx = tx.clone();
                let web3 = web3.clone();
                sub.chunks(interval as usize)
                    .for_each(move |_| {
                        let handle = handle.clone();
                        let tx = tx.clone();
                        let web3 = web3.clone();
                        web3.eth()
                            .block_number()
                            .and_then(move |block| {
                                let from = if block.as_u64() < confirmations + LOOKBACK_RANGE  {
                                    0
                                } else {
                                    block.as_u64() - confirmations - LOOKBACK_RANGE
                                };
                                if block.as_u64() < confirmations + LOOKBACK_LEEWAY {
                                    return Err(web3::Error::from_kind(web3::ErrorKind::Msg("Not enough blocks to look back".to_string())));
                                }
                                let to = block.as_u64() - confirmations - LOOKBACK_LEEWAY;
                                info!(
                                    "Looking for logs between {} and {} on {:?}",
                                    from,
                                    to,
                                    network_type,
                                );
                                Ok(FilterBuilder::default()
                                    .address(vec![token_address])
                                    .from_block(BlockNumber::from(from))
                                    .to_block(BlockNumber::from(to))
                                    .topics(
                                        Some(vec![TRANSFER_EVENT_SIGNATURE.into()]),
                                        None,
                                        Some(vec![relay_address.into()]),
                                        None,
                                    ).build())
                            }).and_then(move |filter| {
                                let web3 = web3.clone();
                                web3.clone().eth().logs(filter).and_then(move |logs| {
                                    let tx = tx.clone();
                                    let web3 = web3.clone();
                                    let handle = handle.clone();
                                    info!(
                                        "Found {} transfers on {:?}",
                                        logs.len(),
                                        network_type
                                    );
                                    logs.iter().for_each(|log| {
                                        if Some(true) == log.removed {
                                            warn!("found removed log");
                                            return;
                                        }

                                        log.transaction_hash.map_or_else(
                                            || {
                                                warn!("log missing transaction hash");
                                                return;
                                            },
                                            |tx_hash| {
                                                let tx = tx.clone();
                                                let destination: Address = log.topics[1].into();
                                                let amount: U256 = log.data.0[..32].into();
                                                if destination == Address::zero() {
                                                    info!("found mint. Skipping");
                                                    return;
                                                }
                                                info!("found transfer event in tx hash {:?}", &tx_hash);
                                                handle.spawn(
                                                    web3.eth()
                                                        .transaction_receipt(tx_hash)
                                                        .and_then(move |transaction_receipt| {
                                                            let tx_hash = tx_hash;
                                                            transaction_receipt.map_or_else(
                                                                || {
                                                                    error!(
                                                                        "no receipt found for transaction hash {}",
                                                                        &tx_hash
                                                                    );
                                                                    Ok(())
                                                                },
                                                                |receipt| {
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
                                                                    info!(
                                                                        "transfer event found, checking for approval {}",
                                                                        &transfer
                                                                    );
                                                                    tx.unbounded_send(transfer).unwrap();
                                                                    Ok(())
                                                                },
                                                            )
                                                        }).or_else(|e| {
                                                            error!("error approving transaction: {}", e);
                                                            Ok(())
                                                        }),
                                                );
                                            },
                                        );
                                    });
                                    Ok(())
                                })
                            }).or_else(move |e| {
                                error!("error in {:?} transfer logs: {}", network_type, e);
                                Ok(())
                            })
                    }).or_else(move |e| {
                        error!("error in {:?} transfer logs: {}", network_type, e);
                        Ok(())
                    })
            }).or_else(move |e| {
                error!("error checking every {} blocks: {}", interval, e);
                Ok(())
            });
        handle.spawn(future);
        FindMissedTransfers(rx)
    }
}

impl Stream for FindMissedTransfers {
    type Item = Transfer;
    type Error = ();
    fn poll(&mut self) -> Poll<Option<Self::Item>, Self::Error> {
        self.0.poll()
    }
}

/// Future to handle the Stream of missed transfers by checking them, and approving them
pub struct HandleMissedTransfers(Box<Future<Item = (), Error = ()>>);

impl HandleMissedTransfers {
    /// Returns a newly created HandleMissedTransfers Future
    ///
    /// # Arguments
    ///
    /// * `source` - Network where the missed transfers are captured
    /// * `target` - Network where the transfer will be approved for a withdrawal
    /// * `handle` - Handle to spawn new futures
    pub fn new<T: DuplexTransport + 'static>(
        source: &Network<T>,
        target: &Rc<Network<T>>,
        handle: &reactor::Handle,
    ) -> Self {
        let handle = handle.clone();
        let target = target.clone();
        let future = FindMissedTransfers::new(source, &handle).for_each(move |transfer| {
            let target = target.clone();
            let handle = handle.clone();
            transfer
                .get_withdrawal(&target)
                .and_then(move |withdrawal| {
                    info!("found withdrawal: {:?}", &withdrawal);
                    if withdrawal.processed {
                        info!("skipping already processed transfer: {:?}", &transfer.tx_hash);
                        Ok(None)
                    } else {
                        Ok(Some(transfer))
                    }
                })
                .and_then(move |transfer_option| {
                    let target = target.clone();
                    let handle = handle.clone();
                    if let Some(transfer) = transfer_option {
                        info!("approving missed transfer: {:?}", transfer);
                        handle.spawn(transfer.approve_withdrawal(&target))
                    }
                    Ok(())
                })
                .or_else(move |e| {
                    error!("Error searching for and approving missed transfers: {:?}", e);
                    Ok(())
                })
        });
        HandleMissedTransfers(Box::new(future))
    }
}

impl Future for HandleMissedTransfers {
    type Item = ();
    type Error = ();
    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        self.0.poll()
    }
}
