use ethabi::Token;
use web3::contract::Options;
use web3::futures::prelude::*;
use web3::futures::try_ready;
use web3::types::{Address, BlockNumber, H256, U256};
use web3::DuplexTransport;

use super::transfer::Transfer;
use super::ApproveParams;
use crate::eth::transaction::SendTransaction;
use crate::extensions::removed::CancelRemoved;
use crate::relay::Network;

pub enum DoesRequireApprovalState {
    GetFees(Box<dyn Future<Item = U256, Error = ()>>),
    GetWithdrawal(U256, Box<dyn Future<Item = (Address, U256, bool), Error = ()>>),
    GetWithdrawalApprovals(usize, GetWithdrawalApprovals),
}

/// Future to check whether or not a withdrawal needs approval
pub struct DoesRequireApproval<T: DuplexTransport + 'static> {
    target: Network<T>,
    transfer: Transfer,
    state: DoesRequireApprovalState,
}

impl<T: DuplexTransport + 'static> DoesRequireApproval<T> {
    pub fn new(target: &Network<T>, transfer: &Transfer, fees: Option<U256>) -> Self {
        let state = match fees {
            Some(fee) => DoesRequireApprovalState::GetWithdrawal(
                fee,
                Box::new(DoesRequireApproval::get_withdrawal(target, transfer)),
            ),
            None => DoesRequireApprovalState::GetFees(Box::new(DoesRequireApproval::get_fees(target))),
        };

        DoesRequireApproval {
            target: target.clone(),
            transfer: *transfer,
            state,
        }
    }

    pub fn get_fees(target: &Network<T>) -> impl Future<Item = U256, Error = ()> {
        target
            .relay
            .query("fees", (), target.account, Options::default(), BlockNumber::Latest)
            .map_err(|e| {
                error!("error getting relay fees: {:?}", e);
            })
    }

    pub fn get_withdrawal(
        target: &Network<T>,
        transfer: &Transfer,
    ) -> impl Future<Item = (Address, U256, bool), Error = ()> {
        let approval_hash = transfer.get_withdrawal_hash();
        let network_type = target.network_type;
        target
            .relay
            .query(
                "withdrawals",
                approval_hash,
                None,
                Options::default(),
                BlockNumber::Latest,
            )
            .map_err(move |e| {
                error!("error getting withdrawal on {:?}: {:?}", network_type, e);
            })
    }
}

impl<T: DuplexTransport + 'static> Future for DoesRequireApproval<T> {
    type Item = bool;
    type Error = ();

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        let network_type = self.target.network_type;
        let target = self.target.clone();
        let transfer = self.transfer;
        loop {
            let next = match self.state {
                DoesRequireApprovalState::GetFees(ref mut future) => {
                    let fees = try_ready!(future.poll());
                    debug!("{:?} fees are: {}", network_type, fees);
                    if fees >= transfer.amount {
                        warn!("transaction amount {} below fees {}", transfer.amount, fees);
                        return Ok(Async::Ready(false));
                    }
                    DoesRequireApprovalState::GetWithdrawal(
                        fees,
                        Box::new(DoesRequireApproval::get_withdrawal(&target, &transfer)),
                    )
                }
                DoesRequireApprovalState::GetWithdrawal(fees, ref mut future) => {
                    let withdrawal = try_ready!(future.poll());
                    if (withdrawal.0 == Address::zero() && withdrawal.1.as_u64() == 0)
                        || (withdrawal.0 == self.transfer.destination && withdrawal.1 == self.transfer.amount - fees)
                    {
                        info!("retrieved withdrawal on target {:?}: {:?}", network_type, withdrawal);
                        if withdrawal.2 {
                            return Ok(Async::Ready(false));
                        } else {
                            debug!("transaction not processed on {:?} - checking approvers", network_type);
                            let approval_hash = self.transfer.get_withdrawal_hash();
                            let approval_future = GetWithdrawalApprovals::new(&self.target, &approval_hash, &0.into());
                            DoesRequireApprovalState::GetWithdrawalApprovals(0, approval_future)
                        }
                    } else {
                        error!(
                            "withdrawal from contract did not match transfer on {:?}: {:?} {:?}",
                            network_type, self.transfer, withdrawal
                        );
                        return Ok(Async::Ready(false));
                    }
                }
                DoesRequireApprovalState::GetWithdrawalApprovals(index, ref mut future) => {
                    let polled = future.poll();
                    match polled {
                        Ok(Async::Ready(approval)) => {
                            let account = self.target.account;
                            if approval == account {
                                return Ok(Async::Ready(false));
                            } else {
                                let i = index + 1;
                                let approval_hash = self.transfer.get_withdrawal_hash();
                                let approval_future =
                                    GetWithdrawalApprovals::new(&self.target, &approval_hash, &i.into());
                                DoesRequireApprovalState::GetWithdrawalApprovals(i, approval_future)
                            }
                        }
                        Ok(Async::NotReady) => {
                            return Ok(Async::NotReady);
                        }
                        Err(()) => {
                            debug!("have not approved transaction");
                            return Ok(Async::Ready(true));
                        }
                    }
                }
            };
            self.state = next;
        }
    }
}

pub struct GetWithdrawalApprovals(Box<dyn Future<Item = Address, Error = ()>>);

impl GetWithdrawalApprovals {
    pub fn new<T: DuplexTransport + 'static>(target: &Network<T>, approval_hash: &H256, index: &U256) -> Self {
        let target = target.clone();
        let network_type = target.network_type;
        let index = *index;
        let approval_hash = *approval_hash;
        let future = Box::new(
            target
                .relay
                .query(
                    "withdrawalApprovals",
                    vec![Token::FixedBytes(approval_hash.0.to_vec()), Token::Uint(index)],
                    None,
                    Options::default(),
                    BlockNumber::Latest,
                )
                .map_err(move |_| {
                    // We expect this error
                    info!(
                        "found all {} approvers for {:?} on {:?}",
                        index, network_type, approval_hash
                    );
                }),
        );
        GetWithdrawalApprovals(future)
    }
}

impl Future for GetWithdrawalApprovals {
    type Item = Address;
    type Error = ();

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        self.0.poll()
    }
}

pub struct WaitForWithdrawalProcessed<T: DuplexTransport + 'static> {
    target: Network<T>,
    transfer: Transfer,
    future: Box<dyn Future<Item = (Address, U256, bool), Error = ()>>,
}

impl<T: DuplexTransport + 'static> WaitForWithdrawalProcessed<T> {
    pub fn new(target: &Network<T>, transfer: &Transfer) -> Self {
        let future = WaitForWithdrawalProcessed::check(target, transfer);
        WaitForWithdrawalProcessed {
            target: target.clone(),
            transfer: *transfer,
            future,
        }
    }

    fn check(target: &Network<T>, transfer: &Transfer) -> Box<dyn Future<Item = (Address, U256, bool), Error = ()>> {
        let approval_hash = transfer.get_withdrawal_hash();
        let network_type = target.network_type;
        Box::new(
            target
                .relay
                .query(
                    "withdrawals",
                    approval_hash,
                    None,
                    Options::default(),
                    BlockNumber::Latest,
                )
                .map_err(move |e| {
                    error!("error getting withdrawal on {:?}: {:?}", network_type, e);
                }),
        )
    }
}

impl<T: DuplexTransport + 'static> Future for WaitForWithdrawalProcessed<T> {
    type Item = ();
    type Error = ();
    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        loop {
            let withdrawal = try_ready!(self.future.poll());
            if withdrawal.2 {
                return Ok(Async::Ready(()));
            } else {
                self.future = WaitForWithdrawalProcessed::check(&self.target, &self.transfer);
            }
        }
    }
}

pub enum ApproveWithdrawalState {
    CheckFlush,
    CheckFees(Box<dyn Future<Item = U256, Error = ()>>),
    SendTransaction(Box<dyn Future<Item = Option<()>, Error = ()>>),
}

pub struct ApproveWithdrawal<T: DuplexTransport + 'static> {
    state: ApproveWithdrawalState,
    transfer: Transfer,
    source: Network<T>,
    target: Network<T>,
}

impl<T: DuplexTransport + 'static> ApproveWithdrawal<T> {
    pub fn new(source: &Network<T>, target: &Network<T>, transfer: &Transfer) -> Self {
        let state = ApproveWithdrawalState::CheckFlush;
        ApproveWithdrawal {
            source: source.clone(),
            target: target.clone(),
            transfer: *transfer,
            state,
        }
    }

    fn check_flushed(&self, chain: &Network<T>) -> Result<bool, ()> {
        Ok(chain.flushed.read().map_err(|e| {
            error!("error getting lock: {:?}", e);
            ()
        })?.is_some())
    }
}

impl<T: DuplexTransport + 'static> Future for ApproveWithdrawal<T> {
    type Item = ();
    type Error = ();

    fn poll(&mut self) -> Result<Async<Self::Item>, Self::Error> {
        let source = self.source.clone();
        let target = self.target.clone();
        let transfer = self.transfer;
        loop {
            let next = match self.state {
                ApproveWithdrawalState::CheckFlush => {
                    if self.check_flushed(&source)? || self.check_flushed(&target)? {
                        warn!("cannot approve withdrawal after flush: {} ", transfer);
                        continue;
                    } else {
                        let future = target
                        .relay
                        .query("fees", (), target.account, Options::default(), BlockNumber::Latest)
                        .map_err(|e| {
                            error!("error getting fees: {:?}", e);
                        });
                        ApproveWithdrawalState::CheckFees(Box::new(future))
                    }
                },
                ApproveWithdrawalState::CheckFees(ref mut future) => {
                    let fees = try_ready!(future.poll());
                    if fees >= transfer.amount {
                        warn!("transaction amount {} below fees {}", transfer.amount, fees);
                        return Ok(Async::Ready(()));
                    } else {
                        let future = SendTransaction::new(
                            &target,
                            "approveWithdrawal",
                            &ApproveParams::from(transfer),
                            target.retries,
                        )
                        .cancel_removed(&source, transfer.tx_hash);
                        ApproveWithdrawalState::SendTransaction(Box::new(future))
                    }
                }
                ApproveWithdrawalState::SendTransaction(ref mut future) => {
                    let success = try_ready!(future.poll());
                    if success.is_none() {
                        warn!(
                            "log removed from originating chain while waiting on approval confirmations on target {:?}",
                            target.network_type
                        );
                    }
                    return Ok(Async::Ready(()));
                }
            };

            self.state = next;
        }
    }
}
