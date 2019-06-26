use std::fmt;
use std::rc::Rc;

use std::time;
use tokio_core::reactor;
use web3::confirm::{wait_for_transaction_confirmation, SendTransactionWithConfirmation};
use web3::futures::prelude::*;
use web3::futures::sync::mpsc;
use web3::futures::try_ready;
use web3::types::{Address, FilterBuilder, TransactionReceipt, H256, U256};
use web3::DuplexTransport;

use super::contracts::TRANSFER_EVENT_SIGNATURE;
use super::relay::{Network, TransactionApprovalState};
use super::transaction::SendTransaction;
use super::withdrawal::{ApproveParams, DoesRequireApproval, UnapproveParams};
use utils::{CheckLogRemoved, CheckRemoved};
use web3::contract::tokens::Tokenize;

/// Add CheckRemoved trait to SendTransaction, which is called by Transfer::approve_withdrawal
impl<T, P> CheckRemoved<T, (), ()> for SendTransaction<T, P>
where
    T: DuplexTransport + 'static,
    P: Tokenize + Clone + 'static,
{
    fn check_log_removed(self, target: &Rc<Network<T>>, tx_hash: H256) -> CheckLogRemoved<T, (), ()> {
        CheckLogRemoved::new(target.clone(), tx_hash, Box::new(self))
    }
}

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

/// Represents a token transfer between two networks
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct Transfer {
    pub destination: Address,
    pub amount: U256,
    pub tx_hash: H256,
    pub block_hash: H256,
    pub block_number: U256,
    pub removed: bool,
}

impl Transfer {
    /// Returns a Transfer object based on a receipt, and some values
    ///
    /// # Arguments
    ///
    /// * `address` Address of the wallet that sent the transfer
    /// * `amount` - Amount of ERC20 token sent
    /// * `removed` - Was this log removed
    /// * `receipt` - TransactionReceipt for this log
    ///
    pub fn from_receipt(
        destination: Address,
        amount: U256,
        removed: bool,
        receipt: &TransactionReceipt,
    ) -> Result<Self, String> {
        if receipt.block_number.is_none() {
            return Err("no block number in transfer receipt".to_string());
        }

        if receipt.block_hash.is_none() {
            return Err("no block hash in transfer receipt".to_string());
        }

        let block_hash = receipt.block_hash.unwrap();
        let block_number = receipt.block_number.unwrap();

        Ok(Transfer {
            destination,
            amount,
            tx_hash: receipt.transaction_hash,
            block_hash,
            block_number,
            removed,
        })
    }
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
    ) -> Box<Future<Item = (), Error = ()>> {
        info!("approving withdrawal on {:?}: {} ", target.network_type, self);
        let target = target.clone();
        Box::new(
            SendTransaction::new(
                &target,
                "approveWithdrawal",
                &ApproveParams::from(*self),
                target.retries,
            )
            .check_log_removed(&target, self.tx_hash)
            .and_then(move |success| {
                success.map_or_else(
                    || {
                        warn!(
                            "log removed from originating chain while waiting on approval confirmations on target {:?}",
                            target.network_type
                        );
                        Ok(())
                    },
                    |_| Ok(()),
                )
            }),
        )
    }

    pub fn unapprove_withdrawal<T: DuplexTransport + 'static>(
        &self,
        target: &Rc<Network<T>>,
    ) -> SendTransaction<T, UnapproveParams> {
        info!("unapproving withdrawal on {:?}: {} ", target.network_type, self);
        SendTransaction::new(
            target,
            "unapproveWithdrawal",
            &UnapproveParams::from(*self),
            target.retries,
        )
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

/// Stream of transfer events that have been on the main chain for N blocks.
/// N is confirmations per settings.
pub struct WatchTransferLogs<T: DuplexTransport + 'static> {
    handle: reactor::Handle,
    rx: mpsc::UnboundedReceiver<Transfer>,
    target: Rc<Network<T>>,
}

impl<T: DuplexTransport + 'static> WatchTransferLogs<T> {
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

        WatchTransferLogs {
            handle: handle.clone(),
            rx,
            target: target.clone(),
        }
    }
}

impl<T: DuplexTransport + 'static> Future for WatchTransferLogs<T> {
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
