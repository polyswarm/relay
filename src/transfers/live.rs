use lru::LruCache;
use std::rc::Rc;
use std::sync::{PoisonError, RwLockWriteGuard};
use std::time;
use tokio_core::reactor;
use web3::confirm::{wait_for_transaction_confirmation, SendTransactionWithConfirmation};
use web3::futures::future::{err, ok, Either, Future};
use web3::futures::prelude::*;
use web3::futures::sync::mpsc;
use web3::futures::try_ready;
use web3::types::{Address, Filter, FilterBuilder, Log, TransactionReceipt, H256, U256};
use web3::{DuplexTransport, ErrorKind};

use super::extensions::removed::{CancelRemoved, ExitOnLogRemoved};
use super::extensions::timeout::SubscriptionState;
use super::relay::{Network, NetworkType, TransferApprovalState};
use super::transfers::transfer::Transfer;

/// Stream of transfer events that have been on the main chain for N blocks.
/// N is confirmations per settings.
pub struct WatchLiveLogs<T: DuplexTransport + 'static> {
    // TODO Add the desired event signature & change name to just WatchLogs
    state: SubscriptionState<T, Log>,
    handle: reactor::Handle,
    source: Network<T>,
    target: Network<T>,
    tx: mpsc::UnboundedSender<(Log, TransactionReceipt)>,
}

impl<T: DuplexTransport + 'static> WatchLiveLogs<T> {
    /// Returns a newly created WatchTransfers Stream
    ///
    /// # Arguments
    ///
    /// * `source` - Network where the transfers are performed
    /// * `handle` - Handle to spawn naew futures
    pub fn new(
        source: &Network<T>,
        target: &Network<T>,
        filter: &Filter,
        tx: &mpsc::UnboundedSender<(Log, TransactionReceipt)>,
        handle: &reactor::Handle,
    ) -> Self {
        let future = Box::new(source.web3.clone().eth_subscribe().subscribe_logs(filter.clone()));

        WatchLiveLogs {
            source: source.clone(),
            target: target.clone(),
            state: SubscriptionState::Subscribing(future),
            handle: handle.clone(),
            tx: tx.clone(),
        }
    }

    fn process_log(&self, log: &Log) -> Box<Future<Item = (), Error = ()>> {
        let network_type = self.source.network_type;
        let target = self.target.clone();
        let source = self.source.clone();
        let tx = self.tx.clone();
        let removed = log.removed.unwrap_or(false);
        let log = log.clone();
        log.transaction_hash.map_or_else(
            || {
                warn!("log missing transaction hash on {:?}", source.network_type);
                Box::new(Either::A(ok(())))
            },
            |tx_hash| {
                let future = source
                    .get_receipt(removed, tx_hash)
                    .and_then(move |receipt_option| receipt_option.ok_or(()))
                    .and_then(move |receipt| {
                        tx.unbounded_send((log.clone(), receipt)).unwrap();
                        Ok(())
                    })
                    .map_err(move |_| {
                        error!("error processing log on {:?}", network_type);
                        ()
                    });
                Box::new(Either::B(future))
            },
        )
    }
}

impl<T: DuplexTransport + 'static> Future for WatchLiveLogs<T> {
    type Item = ();
    type Error = web3::Error;
    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        loop {
            let handle = self.handle.clone();
            let tx = self.tx.clone();
            let next = match self.state {
                SubscriptionState::Subscribing(ref mut future) => {
                    let stream = try_ready!(future.poll());
                    Some(SubscriptionState::Subscribed(stream))
                }
                SubscriptionState::Subscribed(ref mut stream) => {
                    match stream.poll() {
                        Ok(Async::Ready(Some(log))) => {
                            // On receiving a valid log, spawn the waiting for a receipt, as it takes
                            // 20 blocks which is 20 seconds or 300 seconds and we don't want to block
                            handle.spawn(self.process_log(&log));
                        }
                        Ok(Async::Ready(None)) => {
                            self.tx.close().map_err(move |_| {
                                web3::Error::from_kind(ErrorKind::Msg("Unable to close sender".to_string()))
                            })?;
                            return Ok(Async::Ready(()));
                        }
                        Ok(Async::NotReady) => {
                            return Ok(Async::NotReady);
                        }
                        Err(e) => {
                            error!("error reading transfer logs on {:?}. {:?}", self.source.network_type, e);
                            self.tx.close().map_err(move |_| {
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

pub struct ProcessTransfer<T: DuplexTransport + 'static> {
    rx: mpsc::UnboundedReceiver<(Log, TransactionReceipt)>,
    handle: reactor::Handle,
    source: Network<T>,
    target: Network<T>,
}

impl<T: DuplexTransport + 'static> ProcessTransfer<T> {
    pub fn new(
        source: &Network<T>,
        target: &Network<T>,
        rx: mpsc::UnboundedReceiver<(Log, TransactionReceipt)>,
        handle: &reactor::Handle,
    ) -> Self {
        ProcessTransfer {
            rx,
            source: source.clone(),
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
                    self.source
                        .pending
                        .write()?
                        .put(transfer.tx_hash, TransferApprovalState::Removed);
                    self.handle.spawn(transfer.unapprove_withdrawal(&self.target));
                }
            }
            Some(TransferApprovalState::Removed) => {
                // Remove logs can be added again
                if !transfer.removed {
                    self.source
                        .pending
                        .write()?
                        .put(transfer.tx_hash, TransferApprovalState::Sent);
                    self.handle
                        .spawn(transfer.approve_withdrawal(&self.source, &self.target));
                }
            }
            None => {
                if transfer.removed {
                    // Write removed state
                    self.source
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
                    self.source
                        .pending
                        .write()?
                        .put(transfer.tx_hash, TransferApprovalState::Sent);

                    // LRU Cache will drop values, so we need to recheck the chain
                    let source = self.source.clone();
                    let target = self.target.clone();
                    let approve_future = transfer.check_withdrawal(&self.target).and_then(move |not_approved| {
                        if not_approved {
                            Either::A(transfer.approve_withdrawal(&source, &target))
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
        let network_type = self.source.network_type;
        loop {
            let next = try_ready!(self.rx.poll());
            match next {
                Some((log, receipt)) => {
                    let destination: Address = log.topics[1].into();
                    let amount: U256 = log.data.0[..32].into();
                    let removed = log.removed.unwrap_or(false);
                    let transfer = Transfer::from_receipt(destination, amount, removed, &receipt).map_err(|e| {
                        error!("error getting transfer from receipt {:?}: {:?}", receipt, e);
                    })?;
                    info!(
                        "transfer event on {:?} confirmed, approving {}",
                        network_type, &transfer
                    );
                    let value = {
                        let lock = self.source.pending.read().map_err(|e| {
                            error!("Failed to acquire read lock {:?}", e);
                        })?;

                        lock.peek(&transfer.tx_hash).copied()
                    };
                    self.advance_transfer_approval(transfer, value).map_err(|e| {
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
        mock.new_network(NetworkType::Home).unwrap();
        mock.new_network(NetworkType::Side).unwrap();
    }

    #[test]
    fn advance_transfer_approval_should_change_transfer_state_to_sent_on_first_non_removal() {
        // arrange
        let eloop = tokio_core::reactor::Core::new().unwrap();
        let handle = eloop.handle();
        let mock = MockTransport::new();
        let source = mock.new_network(NetworkType::Home).unwrap();
        let target = mock.new_network(NetworkType::Side).unwrap();
        let (tx, rx) = mpsc::unbounded();
        let processor = ProcessTransfer::new(&source, &target, rx, &handle);
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
            source.pending.read().unwrap().peek(&transfer.tx_hash),
            Some(&TransferApprovalState::Sent)
        );
    }

    #[test]
    fn advance_transfer_approval_should_do_nothing_with_sent_repeat_transfer() {
        // arrange
        let eloop = tokio_core::reactor::Core::new().unwrap();
        let handle = eloop.handle();
        let mock = MockTransport::new();
        let source = mock.new_network(NetworkType::Home).unwrap();
        let target = mock.new_network(NetworkType::Side).unwrap();
        let (tx, rx) = mpsc::unbounded();
        let processor = ProcessTransfer::new(&source, &target, rx, &handle);
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
        assert_eq!(source.pending.read().unwrap().peek(&transfer.tx_hash), None);
    }

    #[test]
    fn advance_transfer_approval_should_sent_with_previously_removed_repeat_transfer() {
        // arrange
        let eloop = tokio_core::reactor::Core::new().unwrap();
        let handle = eloop.handle();
        let mock = MockTransport::new();
        let source = mock.new_network(NetworkType::Home).unwrap();
        let target = mock.new_network(NetworkType::Side).unwrap();
        let (tx, rx) = mpsc::unbounded();
        let processor = ProcessTransfer::new(&source, &target, rx, &handle);
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
            source.pending.read().unwrap().peek(&transfer.tx_hash),
            Some(&TransferApprovalState::Sent)
        );
    }

    #[test]
    fn advance_transfer_approval_should_remove_sent_on_remove() {
        // arrange
        let eloop = tokio_core::reactor::Core::new().unwrap();
        let handle = eloop.handle();
        let mock = MockTransport::new();
        let source = mock.new_network(NetworkType::Home).unwrap();
        let target = mock.new_network(NetworkType::Side).unwrap();
        let (tx, rx) = mpsc::unbounded();
        let processor = ProcessTransfer::new(&source, &target, rx, &handle);
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
            source.pending.read().unwrap().peek(&transfer.tx_hash),
            Some(&TransferApprovalState::Removed)
        );
    }

    #[test]
    fn advance_transfer_approval_should_remove_none_on_remove() {
        // arrange
        let eloop = tokio_core::reactor::Core::new().unwrap();
        let handle = eloop.handle();
        let mock = MockTransport::new();
        let source = mock.new_network(NetworkType::Home).unwrap();
        let target = mock.new_network(NetworkType::Side).unwrap();
        let (tx, rx) = mpsc::unbounded();
        let processor = ProcessTransfer::new(&source, &target, rx, &handle);
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
            source.pending.read().unwrap().peek(&transfer.tx_hash),
            Some(&TransferApprovalState::Removed)
        );
    }

    #[test]
    fn advance_transfer_approval_should_do_nothing_on_removed_repeat_remove() {
        // arrange
        let eloop = tokio_core::reactor::Core::new().unwrap();
        let handle = eloop.handle();
        let mock = MockTransport::new();
        let source = mock.new_network(NetworkType::Home).unwrap();
        let target = mock.new_network(NetworkType::Side).unwrap();
        let (tx, rx) = mpsc::unbounded();
        let processor = ProcessTransfer::new(&source, &target, rx, &handle);
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
        assert_eq!(source.pending.read().unwrap().peek(&transfer.tx_hash), None);
    }
}
