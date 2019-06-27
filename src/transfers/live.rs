use std::rc::Rc;
use std::time;
use tokio_core::reactor;
use web3::confirm::{wait_for_transaction_confirmation, SendTransactionWithConfirmation};
use web3::futures::prelude::*;
use web3::futures::sync::mpsc;
use web3::futures::try_ready;
use web3::types::{Address, FilterBuilder, TransactionReceipt, H256, U256};
use web3::DuplexTransport;

use super::relay::{Network, TransactionApprovalState};
use super::transactions::contracts::TRANSFER_EVENT_SIGNATURE;
use super::transfers::transfer::Transfer;
use super::utils::{CheckLogRemoved, CheckRemoved};

/// Add CheckRemoved trait to SendTransactionWithConfirmation, which is returned by wait_for_transaction_confirmation()
impl<T> CheckRemoved<T, TransactionReceipt, web3::Error> for SendTransactionWithConfirmation<T>
where
    T: DuplexTransport + 'static,
{
    fn check_log_removed(
        self,
        target: &Rc<Network<T>>,
        tx_hash: H256,
    ) -> CheckLogRemoved<T, TransactionReceipt, web3::Error> {
        CheckLogRemoved::new(target.clone(), tx_hash, Box::new(self))
    }
}

/// Stream of transfer events that have been on the main chain for N blocks.
/// N is confirmations per settings.
pub struct WatchLiveTransfers<T: DuplexTransport + 'static> {
    handle: reactor::Handle,
    rx: mpsc::UnboundedReceiver<Transfer>,
    target: Rc<Network<T>>,
}

impl<T: DuplexTransport + 'static> WatchLiveTransfers<T> {
    /// Returns a newly created WatchTransfers Stream
    ///
    /// # Arguments
    ///
    /// * `source` - Network where the transfers are performed
    /// * `handle` - Handle to spawn new futures
    pub fn new(source: &Network<T>, target: &Rc<Network<T>>, handle: &reactor::Handle) -> Self {
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
            let web3 = source.web3.clone();
            let target = target.clone();
            source
                .web3
                .eth_subscribe()
                .subscribe_logs(filter)
                .and_then(move |sub| {
                    sub.for_each(move |log| {
                        log.transaction_hash.map_or_else(
                            || {
                                warn!("log missing transaction hash on {:?}", network_type);
                                Ok(())
                            },
                            |tx_hash| {
                                let tx = tx.clone();
                                let destination: Address = log.topics[1].into();
                                let amount: U256 = log.data.0[..32].into();

                                if Some(true) == log.removed {
                                    handle.spawn(
                                        web3.eth()
                                            .transaction_receipt(tx_hash)
                                            .and_then(move |receipt_option| {
                                                receipt_option.map_or_else(
                                                    || {
                                                        error!(
                                                            "no receipt found for transaction hash {} on {:?}",
                                                            &tx_hash, network_type
                                                        );
                                                        Ok(())
                                                    },
                                                    |receipt| {
                                                        let transfer_result =
                                                            Transfer::from_receipt(destination, amount, true, &receipt);
                                                        match transfer_result {
                                                            Ok(transfer) => {
                                                                info!(
                                                                    "transfer event on {:?} confirmed, approving {}",
                                                                    network_type, &transfer
                                                                );
                                                                tx.unbounded_send(transfer).unwrap();
                                                            }
                                                            Err(msg) => error!(
                                                                "error producing transfer from receipt on {:?}: {}",
                                                                network_type, msg
                                                            ),
                                                        }
                                                        Ok(())
                                                    },
                                                )
                                            })
                                            .map_err(move |e| {
                                                error!(
                                                    "error getting removed transaction receipt on {:?}: {:?}",
                                                    network_type, e
                                                );
                                            }),
                                    );
                                    return Ok(());
                                }

                                info!(
                                    "received transfer event in tx hash {:?} on {:?}, waiting for confirmations",
                                    &tx_hash, network_type
                                );

                                handle.spawn(
                                    wait_for_transaction_confirmation(
                                        transport.clone(),
                                        tx_hash,
                                        time::Duration::from_secs(1),
                                        confirmations,
                                    )
                                    .check_log_removed(&target, tx_hash)
                                    .and_then(move |receipt_option| {
                                        receipt_option.map_or_else(
                                            || {
                                                warn!(
                                                    "log removed before transaction confirmed on {:?}, skipping",
                                                    network_type
                                                );
                                                Ok(())
                                            },
                                            |receipt| {
                                                let transfer_result =
                                                    Transfer::from_receipt(destination, amount, false, &receipt);

                                                match transfer_result {
                                                    Ok(transfer) => {
                                                        info!(
                                                            "transfer event on {:?} confirmed, approving {}",
                                                            network_type, &transfer
                                                        );
                                                        tx.unbounded_send(transfer).unwrap();
                                                    }
                                                    Err(msg) => error!(
                                                        "error producing transfer from receipt on {:?}: {}",
                                                        network_type, msg
                                                    ),
                                                }

                                                Ok(())
                                            },
                                        )
                                    })
                                    .or_else(move |e| {
                                        error!("error waiting for transfer confirmations on {:?}: {}", network_type, e);
                                        Ok(())
                                    }),
                                );

                                Ok(())
                            },
                        )
                    })
                })
                .map_err(move |e| {
                    error!("error in transfer stream on {:?}: {}", network_type, e);
                })
        };

        handle.spawn(future);

        WatchLiveTransfers {
            handle: handle.clone(),
            rx,
            target: target.clone(),
        }
    }
}

impl<T: DuplexTransport + 'static> Future for WatchLiveTransfers<T> {
    type Item = ();
    type Error = ();
    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        loop {
            let transfer = try_ready!(self.rx.poll());
            if let Some(t) = transfer {
                let state: Option<TransactionApprovalState> = match self.target.pending.read().unwrap().peek(&t.tx_hash)
                {
                    Some(value) => Some(value.clone()),
                    None => None,
                };
                match state {
                    Some(TransactionApprovalState::Approved) => {
                        if t.removed {
                            self.target
                                .pending
                                .write()
                                .unwrap()
                                .put(t.tx_hash, TransactionApprovalState::Removed);
                            self.handle.spawn(t.unapprove_withdrawal(&self.target));
                        }
                    }
                    Some(TransactionApprovalState::WaitApproval) => {
                        if t.removed {
                            self.target
                                .pending
                                .write()
                                .unwrap()
                                .put(t.tx_hash, TransactionApprovalState::Removed);
                            self.handle.spawn(t.unapprove_withdrawal(&self.target));
                        }
                    }
                    Some(TransactionApprovalState::Removed) => {
                        // Remove logs can be added again
                        if !t.removed {
                            self.target
                                .pending
                                .write()
                                .unwrap()
                                .put(t.tx_hash, TransactionApprovalState::WaitApproval);
                            self.handle.spawn(t.approve_withdrawal(&self.target));
                        }
                    }
                    None => {
                        if t.removed {
                            self.target
                                .pending
                                .write()
                                .unwrap()
                                .put(t.tx_hash, TransactionApprovalState::Removed);
                        } else {
                            self.target
                                .pending
                                .write()
                                .unwrap()
                                .put(t.tx_hash, TransactionApprovalState::WaitApproval);
                            self.handle.spawn(t.approve_withdrawal(&self.target));
                        }
                    }
                };
            }
        }
    }
}
