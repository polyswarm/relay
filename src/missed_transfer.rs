use std::rc::Rc;
use tokio_core::reactor;
use web3::futures::prelude::*;
use web3::futures::sync::mpsc;
use web3::futures::try_ready;
use web3::types::{Address, BlockNumber, FilterBuilder, TransactionReceipt, H256, U256};
use web3::DuplexTransport;

use super::contracts::TRANSFER_EVENT_SIGNATURE;
use super::relay::Network;
use super::transfer::Transfer;

pub const LOOKBACK_RANGE: u64 = 1_000;
pub const LOOKBACK_LEEWAY: u64 = 5;

pub enum FindTransferState {
    ExtractTransfers(Box<Future<Item = Vec<Transfer>, Error = ()>>),
    FetchReceipt(Box<Future<Item = Option<TransactionReceipt>, Error = ()>>),
}

/// Future to find a vec of transfers at a specific transaction
pub struct FindTransferInTransaction<T: DuplexTransport + 'static> {
    hash: H256,
    source: Rc<Network<T>>,
    state: FindTransferState,
}

impl<T: DuplexTransport + 'static> FindTransferInTransaction<T> {
    /// Returns a newly created FindTransferInTransaction Future
    ///
    /// # Arguments
    ///
    /// * `source` - Network where the transaction took place
    /// * `hash` - Transaction hash to check
    fn new(source: &Rc<Network<T>>, hash: &H256) -> Self {
        let web3 = source.web3.clone();
        let future = web3.clone().eth().transaction_receipt(*hash).map_err(|e| {
            error!("error getting transaction receipt: {:?}", e);
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
            let hash = self.hash;
            let next = match self.state {
                FindTransferState::ExtractTransfers(ref mut future) => {
                    let transfers = try_ready!(future.poll());
                    if transfers.is_empty() {
                        warn!("no relay transactions found at {}", self.hash);
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
                                error!("receipt did not have block hash");
                                return Err(());
                            }
                            let block_hash = r.block_hash.unwrap();
                            r.block_number.map_or_else(
                                || {
                                    error!("receipt did not have block number");
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
                                                        if log.topics[0] == TRANSFER_EVENT_SIGNATURE.into()
                                                            && log.topics[3] == source.relay.address().into()
                                                        {
                                                            let destination: Address = log.topics[1].into();
                                                            let amount: U256 = log.data.0[..32].into();
                                                            if destination == Address::zero() {
                                                                info!("found mint. Skipping");
                                                                continue;
                                                            }
                                                            let transfer = Transfer {
                                                                destination,
                                                                amount,
                                                                tx_hash: hash,
                                                                block_hash,
                                                                block_number: receipt_block,
                                                            };
                                                            transfers.push(transfer);
                                                        }
                                                    }
                                                }
                                            }
                                            Ok(transfers)
                                        })
                                        .map_err(|e| {
                                            error!("error getting current block: {:?}", e);
                                        });
                                    Ok(FindTransferState::ExtractTransfers(Box::new(future)))
                                },
                            )
                        }
                        None => {
                            error!("Could not find requested transaction hash");
                            return Err(());
                        }
                    }?
                }
            };
            self.state = next;
        }
    }
}

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
            ValidateAndApproveTransfer::new(&target, &handle, &transfer)
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

/// Future to check a transfer against the contract and approve is necessary
pub struct ValidateAndApproveTransfer<T: DuplexTransport + 'static> {
    target: Rc<Network<T>>,
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
    pub fn new(target: &Rc<Network<T>>, handle: &reactor::Handle, transfer: &Transfer) -> Self {
        let future = transfer.check_withdrawal(&target).map_err(|e| {
            error!("error checking withdrawal for approval: {:?}", e);
        });

        ValidateAndApproveTransfer {
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
            info!("approving missed transfer: {:?}", self.transfer);
            handle.spawn(self.transfer.approve_withdrawal(&target));
        }
        Ok(Async::Ready(()))
    }
}
