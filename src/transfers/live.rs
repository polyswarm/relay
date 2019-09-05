use lru::LruCache;
use std::rc::Rc;
use std::sync::{PoisonError, RwLockWriteGuard};
use std::time;
use tokio_core::reactor;
use web3::confirm::{wait_for_transaction_confirmation, SendTransactionWithConfirmation};
use web3::futures::future::{Either, ok};
use web3::futures::prelude::*;
use web3::futures::sync::mpsc;
use web3::futures::try_ready;
use web3::types::{Address, FilterBuilder, Log, TransactionReceipt, H256, U256};
use web3::{DuplexTransport, ErrorKind};

use super::eth::contracts::TRANSFER_EVENT_SIGNATURE;
use super::extensions::removed::{CancelRemoved, ExitOnLogRemoved};
use super::extensions::timeout::SubscriptionState;
use super::relay::{Network, NetworkType, TransferApprovalState};
use super::transfers::transfer::Transfer;

/// Add CheckRemoved trait to SendTransactionWithConfirmation, which is returned by wait_for_transaction_confirmation()
impl<T> CancelRemoved<T, TransactionReceipt, web3::Error> for SendTransactionWithConfirmation<T>
where
    T: DuplexTransport + 'static,
{
    fn cancel_removed(
        self,
        target: &Rc<Network<T>>,
        tx_hash: H256,
    ) -> ExitOnLogRemoved<T, TransactionReceipt, web3::Error> {
        ExitOnLogRemoved::new(target.clone(), tx_hash, Box::new(self))
    }
}

/// Stream of transfer events that have been on the main chain for N blocks.
/// N is confirmations per settings.
pub struct WatchLiveTransfers<T: DuplexTransport + 'static> {
    transaction_hash_processor: TransactionHashProcessor<T>,
    state: SubscriptionState<T, Log>,
    handle: reactor::Handle,
}

impl<T: DuplexTransport + 'static> WatchLiveTransfers<T> {
    /// Returns a newly created WatchTransfers Stream
    ///
    /// # Arguments
    ///
    /// * `source` - Network where the transfers are performed
    /// * `handle` - Handle to spawn naew futures
    pub fn new(
        tx: &mpsc::UnboundedSender<Transfer>,
        source: &Network<T>,
        target: &Rc<Network<T>>,
        handle: &reactor::Handle,
    ) -> Self {
        let filter = FilterBuilder::default()
            .address(vec![source.token.address()])
            .topics(
                Some(vec![TRANSFER_EVENT_SIGNATURE.into()]),
                None,
                Some(vec![source.relay.address().into()]),
                None,
            )
            .build();

        let future = Box::new(source.web3.clone().eth_subscribe().subscribe_logs(filter));

        let transaction_processor = TransactionHashProcessor::new(
            source.network_type,
            source.confirmations,
            &target,
            &source.web3.transport().clone(),
            tx,
        );

        WatchLiveTransfers {
            transaction_hash_processor: transaction_processor,
            state: SubscriptionState::Subscribing(future),
            handle: handle.clone(),
        }
    }

    fn process_log(
        log: &Log,
        transaction_hash_processor: &TransactionHashProcessor<T>,
    ) -> Option<Box<Future<Item = (), Error = ()>>> {
        let destination: Address = log.topics[1].into();
        let amount: U256 = log.data.0[..32].into();
        let removed = log.removed;
        let processor = transaction_hash_processor.clone();
        log.transaction_hash.map_or_else(
            || {
                warn!("log missing transaction hash on {:?}", processor.network_type);
                None
            },
            |tx_hash| Some(processor.process(destination, amount, removed, tx_hash)),
        )
    }
}

impl<T: DuplexTransport + 'static> Future for WatchLiveTransfers<T> {
    type Item = ();
    type Error = web3::Error;
    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        loop {
            let handle = self.handle.clone();
            let processor = self.transaction_hash_processor.clone();
            let next = match self.state {
                SubscriptionState::Subscribing(ref mut future) => {
                    let stream = try_ready!(future.poll());
                    Some(SubscriptionState::Subscribed(stream))
                }
                SubscriptionState::Subscribed(ref mut stream) => {
                    match stream.poll() {
                        Ok(Async::Ready(Some(log))) => {
                            let process_future = WatchLiveTransfers::<T>::process_log(&log, &processor);
                            if let Some(future) = process_future {
                                handle.spawn(future);
                            }
                        }
                        Ok(Async::Ready(None)) => {
                            self.transaction_hash_processor.tx.close().map_err(move |_| {
                                web3::Error::from_kind(ErrorKind::Msg("Unable to close sender".to_string()))
                            })?;
                            return Ok(Async::Ready(()));
                        }
                        Ok(Async::NotReady) => {
                            return Ok(Async::NotReady);
                        }
                        Err(e) => {
                            error!("error reading transfer logs on {:?}. {:?}", processor.network_type, e);
                            self.transaction_hash_processor.tx.close().map_err(move |_| {
                                web3::Error::from_kind(ErrorKind::Msg("Unable to close sender".to_string()))
                            })?;
                            return Err(e);
                        }
                    };
                    None
                }
            };
            if let Some(next_state) = next {
                self.state = next_state;
            }
        }
    }
}

#[derive(Clone)]
pub struct TransactionHashProcessor<T: DuplexTransport + 'static> {
    network_type: NetworkType,
    confirmations: u64,
    transport: T,
    target: Rc<Network<T>>,
    tx: mpsc::UnboundedSender<Transfer>,
}

impl<T: DuplexTransport + 'static> TransactionHashProcessor<T> {
    pub fn new(
        network_type: NetworkType,
        confirmations: u64,
        target: &Rc<Network<T>>,
        transport: &T,
        tx: &mpsc::UnboundedSender<Transfer>,
    ) -> Self {
        TransactionHashProcessor {
            network_type,
            confirmations,
            target: target.clone(),
            transport: transport.clone(),
            tx: tx.clone(),
        }
    }

    fn process(
        &self,
        destination: Address,
        amount: U256,
        removed: Option<bool>,
        transaction_hash: H256,
    ) -> Box<Future<Item = (), Error = ()>> {
        let network_type = self.network_type;
        let target = self.target.clone();
        let transport = self.transport.clone();
        let confirmations = self.confirmations;
        let tx = self.tx.clone();
        let removed = removed.unwrap_or(false);

        let either = if removed {
            Either::A(target.web3.eth().transaction_receipt(transaction_hash))
        } else {
            info!(
                "received transfer event in tx hash {:?} on {:?}, waiting for confirmations",
                &transaction_hash, network_type
            );
            Either::B(
                wait_for_transaction_confirmation(
                    transport,
                    transaction_hash,
                    time::Duration::from_secs(1),
                    confirmations as usize,
                )
                .cancel_removed(&target, transaction_hash),
            )
        };

        Box::new(
            either
                .map_err(move |e| {
                    error!("error checking transaction on {:?}: {:?}", network_type, e);
                })
                .and_then(move |receipt| {
                    let transfer_result = Transfer::from_receipt(destination, amount, removed, &receipt.ok_or(())?);
                    match transfer_result {
                        Ok(transfer) => {
                            info!(
                                "transfer event on {:?} confirmed, approving {}",
                                network_type, &transfer
                            );
                            tx.unbounded_send(transfer).unwrap();
                            Ok(())
                        }
                        Err(msg) => {
                            error!("error producing transfer from receipt on {:?}: {}", network_type, msg);
                            Err(())
                        }
                    }
                })
                .map_err(move |e| {
                    error!(
                        "error getting removed transaction receipt on {:?}: {:?}",
                        network_type, e
                    );
                }),
        )
    }
}

pub struct ProcessTransfer<T: DuplexTransport + 'static> {
    rx: mpsc::UnboundedReceiver<Transfer>,
    handle: reactor::Handle,
    target: Rc<Network<T>>,
}

impl<T: DuplexTransport + 'static> ProcessTransfer<T> {
    pub fn new(rx: mpsc::UnboundedReceiver<Transfer>, target: &Rc<Network<T>>, handle: &reactor::Handle) -> Self {
        ProcessTransfer {
            rx,
            target: target.clone(),
            handle: handle.clone(),
        }
    }

    pub fn advance_transfer_approval(
        &self,
        transfer: Transfer,
        state: Option<TransferApprovalState>,
    ) -> Result<(), PoisonError<RwLockWriteGuard<'_, LruCache<H256, TransferApprovalState>>>> {
        match state {
            Some(TransferApprovalState::Sent) => {
                if transfer.removed {
                    self.target
                        .pending
                        .write()?
                        .put(transfer.tx_hash, TransferApprovalState::Removed);
                    self.handle.spawn(transfer.unapprove_withdrawal(&self.target));
                }
            }
            Some(TransferApprovalState::Removed) => {
                // Remove logs can be added again
                if !transfer.removed {
                    self.target
                        .pending
                        .write()?
                        .put(transfer.tx_hash, TransferApprovalState::Sent);
                    self.handle.spawn(transfer.approve_withdrawal(&self.target));
                }
            }
            None => {
                if transfer.removed {
                    // Write removed state
                    self.target
                        .pending
                        .write()?
                        .put(transfer.tx_hash, TransferApprovalState::Removed);
                    // LRU Cache will drop values, so we need to recheck the chain
                    let target = self.target.clone();
                    let unapprove_future = transfer.check_withdrawal(&self.target).and_then(move |not_approved| {
                        if !not_approved {
                            Either::A(transfer.unapprove_withdrawal(&target))
                        } else {
                            Either::B(ok(()))
                        }
                    });
                    self.handle.spawn(unapprove_future);
                } else {
                    self.target
                        .pending
                        .write()?
                        .put(transfer.tx_hash, TransferApprovalState::Sent);

                    // LRU Cache will drop values, so we need to recheck the chain
                    let target = self.target.clone();
                    let approve_future = transfer.check_withdrawal(&self.target).and_then(move |not_approved| {
                        if not_approved {
                            Either::A(transfer.approve_withdrawal(&target))
                        } else {
                            Either::B(ok(()))
                        }
                    });
                    self.handle.spawn(approve_future);
                }
            }
        };
        Ok(())
    }
}

impl<T: DuplexTransport + 'static> Future for ProcessTransfer<T> {
    type Item = ();
    type Error = ();
    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        loop {
            let transfer = try_ready!(self.rx.poll());
            match transfer {
                Some(t) => {
                    let value = {
                        let lock = self.target.pending.read().map_err(|e| {
                            error!("Failed to acquire read lock {:?}", e);
                        })?;

                        lock.peek(&t.tx_hash).copied()
                    };
                    self.advance_transfer_approval(t, value).map_err(|e| {
                        error!("Failed to acquire write lock {:?}", e);
                    })?;
                }
                None => {
                    return Ok(Async::Ready(()));
                }
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::transport::MockTransport;
    use web3::types::{H256, U256};

    #[test]
    fn should_build_network_with_mock() {
        let mock = MockTransport::new();
        Rc::new(mock.new_network(NetworkType::Home).unwrap());
        Rc::new(mock.new_network(NetworkType::Side).unwrap());
    }

    #[test]
    fn process_transfer_should_process_create_transfer_state_on_tx() {
        // arrange
        let mut eloop = tokio_core::reactor::Core::new().unwrap();
        let handle = eloop.handle();
        let mock = MockTransport::new();
        let target = Rc::new(mock.new_network(NetworkType::Side).unwrap());
        let (tx, rx) = mpsc::unbounded();
        let processor = ProcessTransfer::new(rx, &target, &handle);
        let transfer = Transfer {
            destination: Address::zero(),
            amount: U256::zero(),
            tx_hash: H256::zero(),
            block_hash: H256::zero(),
            block_number: U256::zero(),
            removed: false,
        };
        // act
        let send_and_close = move |mut tx: mpsc::UnboundedSender<Transfer>| {
            tx.unbounded_send(transfer).unwrap();
            tx.close().unwrap();
        };
        send_and_close(tx);

        // assert
        assert!(eloop.run(processor).is_ok());
        assert!(target.pending.read().unwrap().peek(&transfer.tx_hash).is_some());
    }

    #[test]
    fn advance_transfer_approval_should_change_transfer_state_to_sent_on_first_non_removal() {
        // arrange
        let eloop = tokio_core::reactor::Core::new().unwrap();
        let handle = eloop.handle();
        let mock = MockTransport::new();
        let target = Rc::new(mock.new_network(NetworkType::Side).unwrap());
        let (_tx, rx) = mpsc::unbounded();
        let processor = ProcessTransfer::new(rx, &target, &handle);
        let transfer = Transfer {
            destination: Address::zero(),
            amount: U256::zero(),
            tx_hash: H256::zero(),
            block_hash: H256::zero(),
            block_number: U256::zero(),
            removed: false,
        };
        // act
        processor.advance_transfer_approval(transfer, None).unwrap();
        // assert
        assert_eq!(
            target.pending.read().unwrap().peek(&transfer.tx_hash),
            Some(&TransferApprovalState::Sent)
        );
    }

    #[test]
    fn advance_transfer_approval_should_do_nothing_with_sent_repeat_transfer() {
        // arrange
        let eloop = tokio_core::reactor::Core::new().unwrap();
        let handle = eloop.handle();
        let mock = MockTransport::new();
        let target = Rc::new(mock.new_network(NetworkType::Side).unwrap());
        let (_tx, rx) = mpsc::unbounded();
        let processor = ProcessTransfer::new(rx, &target, &handle);
        let transfer = Transfer {
            destination: Address::zero(),
            amount: U256::zero(),
            tx_hash: H256::zero(),
            block_hash: H256::zero(),
            block_number: U256::zero(),
            removed: false,
        };
        // act
        processor
            .advance_transfer_approval(transfer, Some(TransferApprovalState::Sent))
            .unwrap();
        // assert
        assert_eq!(target.pending.read().unwrap().peek(&transfer.tx_hash), None);
    }

    #[test]
    fn advance_transfer_approval_should_sent_with_previously_removed_repeat_transfer() {
        // arrange
        let eloop = tokio_core::reactor::Core::new().unwrap();
        let handle = eloop.handle();
        let mock = MockTransport::new();
        let target = Rc::new(mock.new_network(NetworkType::Side).unwrap());
        let (_tx, rx) = mpsc::unbounded();
        let processor = ProcessTransfer::new(rx, &target, &handle);
        let transfer = Transfer {
            destination: Address::zero(),
            amount: U256::zero(),
            tx_hash: H256::zero(),
            block_hash: H256::zero(),
            block_number: U256::zero(),
            removed: false,
        };
        // act
        processor
            .advance_transfer_approval(transfer, Some(TransferApprovalState::Removed))
            .unwrap();
        // assert
        assert_eq!(
            target.pending.read().unwrap().peek(&transfer.tx_hash),
            Some(&TransferApprovalState::Sent)
        );
    }

    #[test]
    fn advance_transfer_approval_should_remove_sent_on_remove() {
        // arrange
        let eloop = tokio_core::reactor::Core::new().unwrap();
        let handle = eloop.handle();
        let mock = MockTransport::new();
        let target = Rc::new(mock.new_network(NetworkType::Side).unwrap());
        let (_tx, rx) = mpsc::unbounded();
        let processor = ProcessTransfer::new(rx, &target, &handle);
        let transfer = Transfer {
            destination: Address::zero(),
            amount: U256::zero(),
            tx_hash: H256::zero(),
            block_hash: H256::zero(),
            block_number: U256::zero(),
            removed: true,
        };
        // act
        processor
            .advance_transfer_approval(transfer, Some(TransferApprovalState::Sent))
            .unwrap();
        // assert
        assert_eq!(
            target.pending.read().unwrap().peek(&transfer.tx_hash),
            Some(&TransferApprovalState::Removed)
        );
    }

    #[test]
    fn advance_transfer_approval_should_remove_none_on_remove() {
        // arrange
        let eloop = tokio_core::reactor::Core::new().unwrap();
        let handle = eloop.handle();
        let mock = MockTransport::new();
        let target = Rc::new(mock.new_network(NetworkType::Side).unwrap());
        let (_tx, rx) = mpsc::unbounded();
        let processor = ProcessTransfer::new(rx, &target, &handle);
        let transfer = Transfer {
            destination: Address::zero(),
            amount: U256::zero(),
            tx_hash: H256::zero(),
            block_hash: H256::zero(),
            block_number: U256::zero(),
            removed: true,
        };
        // act
        processor.advance_transfer_approval(transfer, None).unwrap();
        // assert
        assert_eq!(
            target.pending.read().unwrap().peek(&transfer.tx_hash),
            Some(&TransferApprovalState::Removed)
        );
    }

    #[test]
    fn advance_transfer_approval_should_do_nothing_on_removed_repeat_remove() {
        // arrange
        let eloop = tokio_core::reactor::Core::new().unwrap();
        let handle = eloop.handle();
        let mock = MockTransport::new();
        let target = Rc::new(mock.new_network(NetworkType::Side).unwrap());
        let (_tx, rx) = mpsc::unbounded();
        let processor = ProcessTransfer::new(rx, &target, &handle);
        let transfer = Transfer {
            destination: Address::zero(),
            amount: U256::zero(),
            tx_hash: H256::zero(),
            block_hash: H256::zero(),
            block_number: U256::zero(),
            removed: true,
        };
        // act
        processor
            .advance_transfer_approval(transfer, Some(TransferApprovalState::Removed))
            .unwrap();
        // assert
        assert_eq!(target.pending.read().unwrap().peek(&transfer.tx_hash), None);
    }
}