//use actix_web::Either;
//use eth::contracts::{FLUSH_EVENT_SIGNATURE, TRANSFER_EVENT_SIGNATURE};
//use extensions::removed::{CancelRemoved, ExitOnLogRemoved};
//use extensions::timeout::SubscriptionState;
//use relay::{Network, NetworkType, TransferApprovalState};
//use server::endpoint::BalanceResponse;
//use std::collections::HashMap;
//use std::rc::Rc;
//use std::sync::{PoisonError, RwLockWriteGuard};
//use std::time::Instant;
//use std::{cmp, time};
//use tokio::sync::mpsc;
//use tokio_core::reactor;
//use transfers::transfer::Transfer;
//use web3::confirm::{wait_for_transaction_confirmation, SendTransactionWithConfirmation};
//use web3::contract::ErrorKind;
//use web3::futures::prelude::*;
//use web3::types::{Address, BlockNumber, Log, TransactionReceipt, H256, U256};
//use web3::DuplexTransport;
//use lru::LruCache;
//
//impl<T> CancelRemoved<T, TransactionReceipt, web3::Error> for SendTransactionWithConfirmation<T>
//where
//    T: DuplexTransport + 'static,
//{
//    fn cancel_removed(
//        self,
//        target: &Network<T>,
//        tx_hash: H256,
//    ) -> ExitOnLogRemoved<T, TransactionReceipt, web3::Error> {
//        ExitOnLogRemoved::new(target.clone(), tx_hash, Box::new(self))
//    }
//}
//
///// Stream of transfer events that have been on the main chain for N blocks.
///// N is confirmations per settings.
//pub struct WatchFlush<T: DuplexTransport + 'static> {
//    transaction_hash_processor: TransactionHashProcessor<T>,
//    state: SubscriptionState<T, Log>,
//    handle: reactor::Handle,
//}
//
//impl<T: DuplexTransport + 'static> WatchFlush<T> {
//    /// Returns a newly created WatchTransfers Stream
//    ///
//    /// # Arguments
//    ///
//    /// * `source` - Network where the transfers are performed
//    /// * `handle` - Handle to spawn naew futures
//    pub fn new(
//        source: &Network<T>,
//        target: &Network<T>,
//        handle: &reactor::Handle,
//    ) -> Self {
//        let filter = FilterBuilder::default()
//            .address(vec![source.relay.address()])
//            .topics(Some(vec![FLUSH_EVENT_SIGNATURE.into()]), None, None, None)
//            .build();
//
//        let future = Box::new(source.web3.clone().eth_subscribe().subscribe_logs(filter));
//
//        let transaction_processor = TransactionHashProcessor::new(
//            source.network_type,
//            source.confirmations,
//            &target,
//            &source.web3.transport().clone(),
//            tx,
//        );
//
//        WatchFlush {
//            transaction_hash_processor: transaction_processor,
//            state: SubscriptionState::Subscribing(future),
//            handle: handle.clone(),
//        }
//    }
//
//    fn process_log(
//        log: &Log,
//        transaction_hash_processor: &TransactionHashProcessor<T>,
//    ) -> Option<Box<Future<Item = (), Error = ()>>> {
//        let destination: Address = log.topics[1].into();
//        let amount: U256 = log.data.0[..32].into();
//        let removed = log.removed;
//        let processor = transaction_hash_processor.clone();
//        log.transaction_hash.map_or_else(
//            || {
//                warn!("log missing transaction hash on {:?}", processor.network_type);
//                None
//            },
//            |tx_hash| {
//                // Handle confirmed flush here
//                let future = processor.process(destination, amount, removed, tx_hash)
//                .and_then(move |receipt| {
//                    let transfer_result =
//                        Transfer::from_receipt(destination, amount, removed, &receipt.ok_or(())?);
//                    match transfer_result {
//                        Ok(transfer) => {
//                            info!(
//                                "transfer event on {:?} confirmed, approving {}",
//                                network_type, &transfer
//                            );
//                            tx.unbounded_send(transfer).unwrap();
//                            Ok(())
//                        }
//                        Err(msg) => {
//                            error!(
//                                "error producing transfer from receipt on {:?}: {}",
//                                network_type, msg
//                            );
//                            Err(())
//                        }
//                    }
//                })
//                .map_err(move |e| {
//                    error!(
//                        "error getting removed transaction receipt on {:?}: {:?}",
//                        network_type, e
//                    );
//                });
//                Some(future)
//            }
//        )
//    }
//}
//
//impl<T: DuplexTransport + 'static> Future for WatchFlush<T> {
//    type Item = ();
//    type Error = web3::Error;
//    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
//        loop {
//            let handle = self.handle.clone();
//            let processor = self.transaction_hash_processor.clone();
//            let next = match self.state {
//                SubscriptionState::Subscribing(ref mut future) => {
//                    let stream = try_ready!(future.poll());
//                    Some(SubscriptionState::Subscribed(stream))
//                }
//                SubscriptionState::Subscribed(ref mut stream) => {
//                    match stream.poll() {
//                        Ok(Async::Ready(Some(log))) => {
//                            let process_future = WatchFlush::<T>::process_log(&log, &processor);
//                            if let Some(future) = process_future {
//                                // TODO Handle confirmed flush here
//                                // Create new state to get balances
//
//                            }
//                        }
//                        Ok(Async::Ready(None)) => {
//                            self.transaction_hash_processor.tx.close().map_err(move |_| {
//                                web3::Error::from_kind(ErrorKind::Msg("Unable to close sender".to_string()))
//                            })?;
//                            return Ok(Async::Ready(()));
//                        }
//                        Ok(Async::NotReady) => {
//                            return Ok(Async::NotReady);
//                        }
//                        Err(e) => {
//                            error!("error reading transfer logs on {:?}. {:?}", processor.network_type, e);
//                            self.transaction_hash_processor.tx.close().map_err(move |_| {
//                                web3::Error::from_kind(ErrorKind::Msg("Unable to close sender".to_string()))
//                            })?;
//                            return Err(e);
//                        }
//                    };
//                    None
//                }
//            };
//            if let Some(next_state) = next {
//                self.state = next_state;
//            }
//        }
//    }
//}
//
//pub struct ProcessTransfer<T: DuplexTransport + 'static> {
//    rx: mpsc::UnboundedReceiver<Transfer>,
//    handle: reactor::Handle,
//    target: Network<T>,
//}
//
//impl<T: DuplexTransport + 'static> ProcessTransfer<T> {
//    pub fn new(rx: mpsc::UnboundedReceiver<Transfer>, target: &Network<T>, handle: &reactor::Handle) -> Self {
//        ProcessTransfer {
//            rx,
//            target: target.clone(),
//            handle: handle.clone(),
//        }
//    }
//
//    pub fn advance_transfer_approval(
//        &self,
//        transfer: Transfer,
//        state: Option<TransferApprovalState>,
//    ) -> Result<(), PoisonError<RwLockWriteGuard<'_, LruCache<H256, TransferApprovalState>>>> {
//        match state {
//            Some(TransferApprovalState::Sent) => {
//                if transfer.removed {
//                    self.target
//                        .pending
//                        .write()?
//                        .put(transfer.tx_hash, TransferApprovalState::Removed);
//                    self.handle.spawn(transfer.unapprove_withdrawal(&self.target));
//                }
//            }
//            Some(TransferApprovalState::Removed) => {
//                // Remove logs can be added again
//                if !transfer.removed {
//                    self.target
//                        .pending
//                        .write()?
//                        .put(transfer.tx_hash, TransferApprovalState::Sent);
//                    self.handle.spawn(transfer.approve_withdrawal(&self.target));
//                }
//            }
//            None => {
//                if transfer.removed {
//                    // Write removed state
//                    self.target
//                        .pending
//                        .write()?
//                        .put(transfer.tx_hash, TransferApprovalState::Removed);
//                    // LRU Cache will drop values, so we need to recheck the chain
//                    let target = self.target.clone();
//                    let unapprove_future = transfer.check_withdrawal(&self.target).and_then(move |not_approved| {
//                        if !not_approved {
//                            Either::A(transfer.unapprove_withdrawal(&target))
//                        } else {
//                            Either::B(ok(()))
//                        }
//                    });
//                    self.handle.spawn(unapprove_future);
//                } else {
//                    self.target
//                        .pending
//                        .write()?
//                        .put(transfer.tx_hash, TransferApprovalState::Sent);
//
//                    // LRU Cache will drop values, so we need to recheck the chain
//                    let target = self.target.clone();
//                    let approve_future = transfer.check_withdrawal(&self.target).and_then(move |not_approved| {
//                        if not_approved {
//                            Either::A(transfer.approve_withdrawal(&target))
//                        } else {
//                            Either::B(ok(()))
//                        }
//                    });
//                    self.handle.spawn(approve_future);
//                }
//            }
//        };
//        Ok(())
//    }
//}
//
//impl<T: DuplexTransport + 'static> Future for ProcessTransfer<T> {
//    type Item = ();
//    type Error = ();
//    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
//        loop {
//            let transfer = try_ready!(self.rx.poll());
//            match transfer {
//                Some(t) => {
//                    let value = {
//                        let lock = self.target.pending.read().map_err(|e| {
//                            error!("Failed to acquire read lock {:?}", e);
//                        })?;
//
//                        lock.peek(&t.tx_hash).copied()
//                    };
//                    self.advance_transfer_approval(t, value).map_err(|e| {
//                        error!("Failed to acquire write lock {:?}", e);
//                    })?;
//                }
//                None => {
//                    return Ok(Async::Ready(()));
//                }
//            };
//        }
//    }
//}
//
//pub struct IsContract<T: DuplexTransport + 'static> {
//    source: Network<T>,
//    address: Address,
//    future: Vec<U256>,
//}
//
//impl<T: DuplexTransport + 'static> IsContract<T> {
//    pub fn new(source: &Network<T>, wallet: &Address) {
//        // Get contract data
//    }
//}
//
//impl<T: DuplexTransport + 'static> Future for IsContract<T> {
//    type Item = bool;
//    type Error = ();
//    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
//        let bytes = try_ready!(self.future.poll());
//        Ok(Async::Ready(bytes == "0x"))
//    }
//}
//
//pub enum BalanceCheckState {
//    GetEndingBlock(Box<Future<Item = U256, Error = ()>>),
//    GetLogWindow(u64, u64, Box<Future<Item = Vec<Log>, Error = ()>>),
//}
//
//pub struct BalanceCheck<T: DuplexTransport + 'static> {
//    source: Network<T>,
//    state: BalanceCheckState,
//    tx: mpsc::UnboundedSender<Result<BalanceResponse, ()>>,
//    balances: HashMap<Address, U256>,
//    start: Instant,
//}
//
//impl<T: DuplexTransport + 'static> BalanceCheck<T> {
//    fn new(source: &Network<T>, tx: &mpsc::UnboundedSender<Result<BalanceResponse, ()>>) -> Self {
//        let future = source.web3.eth().block_number().map_err(move |e| {
//            error!("error getting block number {:?}", e);
//        });
//
//        BalanceCheck {
//            source: source.clone(),
//            tx: tx.clone(),
//            state: BalanceCheckState::GetEndingBlock(Box::new(future)),
//            balances: HashMap::new(),
//            start: Instant::now(),
//        }
//    }
//
//    fn build_next_window(&self, start: u64, end: u64) -> Box<Future<Item = Vec<Log>, Error = ()>> {
//        let token_address: Address = self.source.token.address();
//        let filter = FilterBuilder::default()
//            .address(vec![token_address])
//            .from_block(BlockNumber::from(start))
//            .to_block(BlockNumber::Number(end))
//            .topics(Some(vec![TRANSFER_EVENT_SIGNATURE.into()]), None, None, None)
//            .build();
//        //self.state = ;
//        let future = self.source.web3.eth().logs(filter).map_err(move |e| {
//            error!("error getting block number {:?}", e);
//        });
//        Box::new(future)
//    }
//}
//
//impl<T: DuplexTransport + 'static> Future for BalanceCheck<T> {
//    type Item = ();
//    type Error = ();
//    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
//        loop {
//            let tx = self.tx.clone();
//            let next = match self.state {
//                BalanceCheckState::GetEndingBlock(ref mut future) => {
//                    let block = try_ready!(future.poll());
//                    let window_end = cmp::min(block.as_u64(), 1000);
//                    let future = self.build_next_window(0, window_end);
//                    BalanceCheckState::GetLogWindow(block.as_u64(), window_end, future)
//                }
//                BalanceCheckState::GetLogWindow(end, window_end, ref mut future) => {
//                    let logs = try_ready!(future.poll());
//                    info!(
//                        "found {} logs with transfers between {} and {}",
//                        logs.len(),
//                        window_end,
//                        end
//                    );
//                    // Process existing logs
//                    logs.iter().for_each(|log| {
//                        if Some(true) == log.removed {
//                            return;
//                        }
//
//                        let sender_address: Address = log.topics[1].into();
//                        let receiver_address: Address = log.topics[2].into();
//                        let amount: U256 = log.data.0[..32].into();
//                        debug!("{} transferred {} to {}", sender_address, amount, receiver_address);
//                        // Don't care if source doesn't exist, because it is likely a mint in that case
//                        let zero = U256::zero();
//                        self.balances
//                            .entry(sender_address)
//                            .and_modify(|v| {
//                                if !v.is_zero() {
//                                    *v -= amount;
//                                }
//                            })
//                            .or_insert(zero);
//                        let dest_balance = self.balances.entry(receiver_address).or_insert(zero);
//                        *dest_balance += amount;
//                    });
//
//                    debug!("Window end is {} of {} blocks", window_end, end);
//                    // Setup next window
//                    if window_end < end {
//                        let next_window_end = cmp::min(end, window_end + 1000);
//                        let future = self.build_next_window(window_end + 1, next_window_end);
//                        BalanceCheckState::GetLogWindow(end, next_window_end, future)
//                    } else {
//                        let send_result = tx.unbounded_send(Ok(BalanceResponse::new(
//                            &Instant::now().duration_since(self.start),
//                            &self.balances.clone(),
//                        )));
//                        if send_result.is_err() {
//                            error!("error sending balance response");
//                        }
//                        return Ok(Async::Ready(()));
//                    }
//                }
//            };
//            self.state = next;
//        }
//    }
//}
