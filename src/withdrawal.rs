use ethabi::Token;
use std::rc::Rc;
use tiny_keccak::keccak256;
use web3::contract;
use web3::contract::tokens::{Detokenize, Tokenize};
use web3::contract::Options;
use web3::futures::prelude::*;
use web3::futures::try_ready;
use web3::types::{Address, BlockNumber, H256, U256};
use web3::DuplexTransport;

use super::relay::Network;
use super::transfer::Transfer;

pub struct ApprovalQuery {
    approval_hash: H256,
    index: U256,
}

impl ApprovalQuery {
    fn new(approval_hash: &H256, index: &U256) -> Self {
        ApprovalQuery {
            approval_hash: *approval_hash,
            index: *index,
        }
    }
}

impl Tokenize for ApprovalQuery {
    fn into_tokens(self) -> Vec<Token> {
        vec![Token::FixedBytes(self.approval_hash.to_vec()), Token::Uint(self.index)]
    }
}

/// Withdrawal event added to contract after a transfer
#[derive(Debug, Clone)]
pub struct Withdrawal {
    pub destination: Address,
    pub amount: U256,
    pub processed: bool,
}

impl Detokenize for Withdrawal {
    /// Creates a new instance from parsed ABI tokens.
    fn from_tokens(tokens: Vec<Token>) -> Result<Self, contract::Error>
    where
        Self: Sized,
    {
        let destination = tokens[0].clone().to_address().ok_or_else(|| {
            contract::Error::from_kind(contract::ErrorKind::Msg(
                "cannot parse destination address from contract response".to_string(),
            ))
        })?;
        let amount = tokens[1].clone().to_uint().ok_or_else(|| {
            contract::Error::from_kind(contract::ErrorKind::Msg(
                "cannot parse amount uint from contract response".to_string(),
            ))
        })?;
        let processed = tokens[2].clone().to_bool().ok_or_else(|| {
            contract::Error::from_kind(contract::ErrorKind::Msg(
                "cannot parse processed bool from contract response".to_string(),
            ))
        })?;
        Ok(Withdrawal {
            destination,
            amount,
            processed,
        })
    }
}

/// Withdrawal event added to contract after a transfer
#[derive(Debug, Clone)]
pub struct WithdrawalApprovals(Address);

impl Detokenize for WithdrawalApprovals {
    /// Creates a new instance from parsed ABI tokens.
    fn from_tokens(tokens: Vec<Token>) -> Result<Self, contract::Error>
    where
        Self: Sized,
    {
        let approval = tokens[0].clone().to_address().ok_or_else(|| {
            contract::Error::from_kind(contract::ErrorKind::Msg(
                "cannot parse approvers from contract response".to_string(),
            ))
        })?;
        info!("withdrawal approval: {:?}", approval);
        Ok(WithdrawalApprovals(approval))
    }
}

pub enum CheckWithdrawalState {
    GetWithdrawal(Box<Future<Item = Withdrawal, Error = ()>>),
    GetWithdrawalApprovals(usize, GetWithdrawalApprovals),
}

/// Future to check whether or not a withdrawal needs approval
pub struct DoesRequireApproval<T: DuplexTransport + 'static> {
    target: Rc<Network<T>>,
    transfer: Transfer,
    state: CheckWithdrawalState,
}

impl<T: DuplexTransport + 'static> DoesRequireApproval<T> {
    pub fn new(target: &Rc<Network<T>>, transfer: &Transfer) -> Self {
        let approval_hash = Self::get_withdrawal_hash(transfer);
        let account = target.account;
        let future = Box::new(
            target
                .relay
                .query::<Withdrawal, Address, BlockNumber, H256>(
                    "withdrawals",
                    approval_hash,
                    account,
                    Options::default(),
                    BlockNumber::Latest,
                )
                .map_err(|e| {
                    error!("error getting withdrawal: {:?}", e);
                }),
        );
        let state = CheckWithdrawalState::GetWithdrawal(future);
        DoesRequireApproval {
            target: target.clone(),
            transfer: *transfer,
            state,
        }
    }

    fn get_withdrawal_hash(transfer: &Transfer) -> H256 {
        let tx_hash = transfer.tx_hash;
        let block_hash = transfer.block_hash;
        let block_number: &mut [u8] = &mut [0; 32];
        transfer.block_number.to_big_endian(block_number);
        let mut grouped: Vec<u8> = Vec::new();
        grouped.extend_from_slice(&tx_hash[..]);
        grouped.extend_from_slice(&block_hash[..]);
        grouped.extend_from_slice(block_number);
        H256(keccak256(&grouped[..]))
    }
}

impl<T: DuplexTransport + 'static> Future for DoesRequireApproval<T> {
    type Item = bool;
    type Error = ();

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        loop {
            let next = match self.state {
                CheckWithdrawalState::GetWithdrawal(ref mut future) => {
                    let withdrawal = try_ready!(future.poll());
                    if (withdrawal.destination == Address::zero() && withdrawal.amount.as_u64() == 0)
                        || (withdrawal.destination == self.transfer.destination
                            && withdrawal.amount == self.transfer.amount)
                    {
                        info!("found withdrawal: {:?}", withdrawal);
                        if withdrawal.processed {
                            return Ok(Async::Ready(false));
                        } else {
                            info!("transaction not processed - checking approvers");
                            let approval_hash = Self::get_withdrawal_hash(&self.transfer);
                            let approval_future = GetWithdrawalApprovals::new(&self.target, &approval_hash, &0.into());
                            CheckWithdrawalState::GetWithdrawalApprovals(0, approval_future)
                        }
                    } else {
                        error!("withdrawal from contract did not match transfer");
                        return Ok(Async::Ready(false));
                    }
                }
                CheckWithdrawalState::GetWithdrawalApprovals(index, ref mut future) => {
                    let polled = future.poll();
                    match polled {
                        Ok(Async::Ready(approval)) => {
                            let account = self.target.account;
                            if approval.0 == account {
                                return Ok(Async::Ready(false));
                            } else {
                                let i = index + 1;
                                let approval_hash = Self::get_withdrawal_hash(&self.transfer);
                                let approval_future =
                                    GetWithdrawalApprovals::new(&self.target, &approval_hash, &i.into());
                                CheckWithdrawalState::GetWithdrawalApprovals(i, approval_future)
                            }
                        }
                        Ok(Async::NotReady) => {
                            return Ok(Async::NotReady);
                        }
                        Err(()) => {
                            info!("this relay missing from approvers");
                            return Ok(Async::Ready(true));
                        }
                    }
                }
            };
            self.state = next;
        }
    }
}

pub struct GetWithdrawalApprovals(Box<Future<Item = WithdrawalApprovals, Error = ()>>);

impl GetWithdrawalApprovals {
    pub fn new<T: DuplexTransport + 'static>(target: &Rc<Network<T>>, approval_hash: &H256, index: &U256) -> Self {
        let target = target.clone();
        let account = target.account;
        let approval_query = ApprovalQuery::new(&approval_hash, &index);
        let approval_hash = *approval_hash;
        let future = Box::new(
            target
                .relay
                .query::<WithdrawalApprovals, Address, BlockNumber, ApprovalQuery>(
                    "withdrawalApprovals",
                    approval_query,
                    account,
                    Options::default(),
                    BlockNumber::Latest,
                )
                .map_err(move |_| {
                    // We expect this error
                    info!("found all approvers for {:?}", approval_hash);
                }),
        );
        GetWithdrawalApprovals(future)
    }
}

impl Future for GetWithdrawalApprovals {
    type Item = WithdrawalApprovals;
    type Error = ();

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        self.0.poll()
    }
}
