use std::rc::Rc;
use tokio_core::reactor;
use web3::futures::future;
use web3::futures::prelude::*;
use web3::futures::sync::mpsc;
use web3::futures::try_ready;
use web3::types::{Address, BlockNumber, FilterBuilder, TransactionReceipt, H256, U256};
use web3::{DuplexTransport, ErrorKind};

use super::eth::contracts::TRANSFER_EVENT_SIGNATURE;
use super::extensions::timeout::Timeout;
use super::relay::Network;
use super::transfer::Transfer;

pub const LOOKBACK_RANGE: u64 = 1_000;
pub const LOOKBACK_LEEWAY: u64 = 5;

/// Stream of Transfer that were missed, either from downtime, or failed approvals
pub struct CheckPastTransfers(mpsc::UnboundedReceiver<Transfer>);

impl CheckPastTransfers {
    /// Returns a newly created FindMissedTransfers Stream
    ///
    /// # Arguments
    ///
    /// * `source` - Network where the missed transfers are found
    /// * `handle` - Handle to spawn new futures
    pub fn new<T: DuplexTransport + 'static>(source: &Network<T>, handle: &reactor::Handle) -> Self {
        let (tx, rx) = mpsc::unbounded();
        let token_address = source.token.address();
        let relay_address = source.relay.address();
        let network_type = source.network_type;
        let interval = source.interval;
        let confirmations = source.confirmations;
        let web3 = source.web3.clone();
        let timeout = source.timeout;

        let future = {
            let handle = handle.clone();

            web3
            .clone()
            .eth_subscribe()
            .subscribe_new_heads()
            .timeout(timeout, &handle)
            .for_each(move |head| {
                    let web3 = web3.clone();
                    let handle = handle.clone();
                    let tx = tx.clone();
                    head.number.map_or_else(|| {
                        warn!("No block number in header on {:?}", network_type);
                        future::Either::A(future::ok(()))
                    },
                    move |block_number| {
                        block_number.checked_rem(interval.into()).map(|u| u.low_u64()).map_or_else(|| {
                            let message = format!("Error computing block_number({}) % interval({})", block_number, interval);
                            future::Either::A(future::err(web3::Error::from_kind(ErrorKind::Msg(message))))
                        }, move |remainder| {
                            if remainder != 0 {
                                return future::Either::A(future::ok(()));
                            }
                            let handle = handle.clone();
                            let tx = tx.clone();
                            let web3 = web3.clone();
                            future::Either::B(web3.eth()
                                .block_number()
                                .and_then(move |block| {
                                    let from = if block.as_u64() < confirmations + LOOKBACK_RANGE  {
                                        0
                                    } else {
                                        block.as_u64() - confirmations - LOOKBACK_RANGE
                                    };
                                    if block.as_u64() < confirmations + LOOKBACK_LEEWAY {
                                        return Err(web3::Error::from_kind(web3::ErrorKind::Msg("Not enough blocks to check".to_string())));
                                    }
                                    let to = block.as_u64() - confirmations - LOOKBACK_LEEWAY;
                                    info!(
                                        "checking logs between {} and {} on {:?}",
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
                                            "found {} transfers on {:?}",
                                            logs.len(),
                                            network_type
                                        );
                                        logs.iter().for_each(|log| {
                                            if Some(true) == log.removed {
                                                warn!("found removed log on {:?}", network_type);
                                                return;
                                            }

                                            log.transaction_hash.map_or_else(
                                                || {
                                                    warn!("log missing transaction hash on {:?}", network_type);
                                                },
                                                |tx_hash| {
                                                    let tx = tx.clone();
                                                    let destination: Address = log.topics[1].into();
                                                    let amount: U256 = log.data.0[..32].into();
                                                    if destination == Address::zero() {
                                                        info!("found mint on {:?}. Skipping", network_type);
                                                        return;
                                                    }
                                                    info!("found transfer event on {:?} in tx hash {:?}", network_type, &tx_hash);
                                                    handle.spawn(
                                                        web3.eth()
                                                            .transaction_receipt(tx_hash)
                                                            .and_then(move |transaction_receipt| {
                                                                let tx_hash = tx_hash;
                                                                transaction_receipt.map_or_else(
                                                                    || {
                                                                        error!(
                                                                            "no receipt found for transaction hash {} on {:?}",
                                                                            &tx_hash,
                                                                            network_type
                                                                        );
                                                                        Ok(())
                                                                    },
                                                                    |receipt| {
                                                                        let transfer_result = Transfer::from_receipt(destination, amount, false, &receipt);

                                                                        match transfer_result {
                                                                            Ok(transfer) => {
                                                                                info!(
                                                                                    "transfer event on {:?} confirmed, approving {}",
                                                                                    network_type, &transfer
                                                                                );
                                                                                tx.unbounded_send(transfer).unwrap();
                                                                            }
                                                                            Err(msg) => {
                                                                                error!("error producing transfer from receipt on {:?}: {}", network_type, msg)
                                                                            }
                                                                        };
                                                                        Ok(())
                                                                })
                                                            }).or_else(move |e| {
                                                                error!("error approving transaction on {:?}: {}", network_type, e);
                                                                Ok(())
                                                            })
                                                    );
                                                },
                                            );
                                        });
                                        Ok(())
                                    })
                                }))
                        })
                    })
            }).map_err(move |e| {
                error!("error in block head stream on {:?}: {:?}", network_type, e);
            })
        };
        handle.spawn(future);
        CheckPastTransfers(rx)
    }
}

impl Stream for CheckPastTransfers {
    type Item = Transfer;
    type Error = ();
    fn poll(&mut self) -> Poll<Option<Self::Item>, Self::Error> {
        self.0.poll()
    }
}

/// Future to handle the Stream of missed transfers by checking them, and approving them
pub struct RecheckPastTransferLogs(Box<Future<Item = (), Error = ()>>);

impl RecheckPastTransferLogs {
    /// Returns a newly created HandleMissedTransfers Future
    ///
    /// # Arguments
    ///
    /// * `source` - Network where the missed transfers are captured
    /// * `target` - Network where the transfer will be approved for a withdrawal
    /// * `handle` - Handle to spawn new futures
    pub fn new<T: DuplexTransport + 'static>(
        source: &Network<T>,
        target: &Network<T>,
        handle: &reactor::Handle,
    ) -> Self {
        let handle = handle.clone();
        let target = target.clone();
        let future = CheckPastTransfers::new(source, &handle)
            .for_each(move |transfer| {
                let target = target.clone();
                let handle = handle.clone();
                ValidateAndApproveTransfer::new(&source, &target, &handle, &transfer)
            })
            .and_then(move |_| {
                // This is only ever triggered when the FindMissedTransfers has an error, and closes the Sender.
                Err(())
            });
        RecheckPastTransferLogs(Box::new(future))
    }
}

impl Future for RecheckPastTransferLogs {
    type Item = ();
    type Error = ();
    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        self.0.poll()
    }
}

/// Future to check a transfer against the contract and approve is necessary
pub struct ValidateAndApproveTransfer<T: DuplexTransport + 'static> {
    source: Network<T>,
    target: Network<T>,
    handle: reactor::Handle,
    transfer: Transfer,
    future: Box<Future<Item = bool, Error = ()>>,
}

impl<T: DuplexTransport + 'static> ValidateAndApproveTransfer<T> {
    /// Returns a newly created HandleMissedTransfers Future
    ///
    /// # Arguments
    ///
    /// * `target` - Network where the transfer will be approved for a withdrawal
    /// * `handle` - Handle to spawn new futures
    /// * `transfer` - Transfer event to check/approve
    pub fn new(source: &Network<T>, target: &Network<T>, handle: &reactor::Handle, transfer: &Transfer) -> Self {
        let network_type = target.network_type;
        let future = transfer.check_withdrawal(&target).map_err(move |e| {
            error!("error checking withdrawal for approval on {:?}: {:?}", network_type, e);
        });

        ValidateAndApproveTransfer {
            source: source.clone(),
            target: target.clone(),
            handle: handle.clone(),
            transfer: *transfer,
            future: Box::new(future),
        }
    }
}

impl<T: DuplexTransport + 'static> Future for ValidateAndApproveTransfer<T> {
    type Item = ();
    type Error = ();
    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        let needs_approval = try_ready!(self.future.poll());
        if needs_approval {
            let target = self.target.clone();
            let handle = self.handle.clone();
            info!(
                "approving missed transfer on {:?}: {:?}",
                target.network_type, self.transfer
            );
            handle.spawn(self.transfer.approve_withdrawal(&target));
        }
        Ok(Async::Ready(()))
    }
}

pub enum FindTransferState {
    ExtractTransfers(Box<Future<Item = Vec<Transfer>, Error = ()>>),
    FetchReceipt(Box<Future<Item = Option<TransactionReceipt>, Error = ()>>),
}

/// Future to find a vec of transfers at a specific transaction
pub struct FindTransferInTransaction<T: DuplexTransport + 'static> {
    hash: H256,
    source: Network<T>,
    state: FindTransferState,
}

impl<T: DuplexTransport + 'static> FindTransferInTransaction<T> {
    /// Returns a newly created FindTransferInTransaction Future
    ///
    /// # Arguments
    ///
    /// * `source` - Network where the transaction took place
    /// * `hash` - Transaction hash to check
    pub fn new(source: &Network<T>, hash: &H256) -> Self {
        let web3 = source.web3.clone();
        let network_type = source.network_type;
        let future = web3.clone().eth().transaction_receipt(*hash).map_err(move |e| {
            error!("error getting transaction receipt on {:?}: {:?}", network_type, e);
        });
        let state = FindTransferState::FetchReceipt(Box::new(future));
        FindTransferInTransaction {
            hash: *hash,
            source: source.clone(),
            state,
        }
    }
}

impl<T: DuplexTransport + 'static> Future for FindTransferInTransaction<T> {
    type Item = Vec<Transfer>;
    type Error = ();
    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        loop {
            let source = self.source.clone();
            let network_type = source.network_type;
            let hash = self.hash;
            let next = match self.state {
                FindTransferState::ExtractTransfers(ref mut future) => {
                    let transfers = try_ready!(future.poll());
                    if transfers.is_empty() {
                        warn!("no relay transactions found on {:?} at {:?}", network_type, hash);
                        return Err(());
                    } else {
                        return Ok(Async::Ready(transfers));
                    }
                }
                FindTransferState::FetchReceipt(ref mut future) => {
                    let receipt = try_ready!(future.poll());
                    match receipt {
                        Some(r) => {
                            if r.block_hash.is_none() {
                                error!("receipt did not have block hash on {:?}", network_type);
                                return Err(());
                            }
                            let block_hash = r.block_hash.unwrap();
                            r.block_number.map_or_else(
                                || {
                                    error!("receipt did not have block number on {:?}", network_type);
                                    Err(())
                                },
                                |receipt_block| {
                                    let future = source
                                        .web3
                                        .clone()
                                        .eth()
                                        .block_number()
                                        .and_then(move |block| {
                                            let mut transfers = Vec::new();
                                            if let Some(confirmed) =
                                                block.checked_rem(receipt_block).map(|u| u.as_u64())
                                            {
                                                if confirmed > source.confirmations {
                                                    let logs = r.logs;
                                                    for log in logs {
                                                        info!(
                                                            "found log at {:?} on {:?}: {:?}",
                                                            hash, network_type, log
                                                        );
                                                        if log.topics[0] == TRANSFER_EVENT_SIGNATURE.into()
                                                            && log.topics[2] == source.relay.address().into()
                                                        {
                                                            let destination: Address = log.topics[1].into();
                                                            let amount: U256 = log.data.0[..32].into();
                                                            if destination == Address::zero() {
                                                                info!("found mint on {:?}. Skipping", network_type);
                                                                continue;
                                                            }
                                                            let transfer = Transfer {
                                                                destination,
                                                                amount,
                                                                tx_hash: hash,
                                                                block_hash,
                                                                block_number: receipt_block,
                                                                removed: false,
                                                            };
                                                            transfers.push(transfer);
                                                        }
                                                    }
                                                }
                                            }
                                            Ok(transfers)
                                        })
                                        .map_err(move |e| {
                                            error!("error getting current block on {:?}: {:?}", network_type, e);
                                        });
                                    Ok(FindTransferState::ExtractTransfers(Box::new(future)))
                                },
                            )
                        }
                        None => {
                            error!("unable to find {:?} on {:?}", hash, source.network_type);
                            return Err(());
                        }
                    }?
                }
            };
            self.state = next;
        }
    }
}
