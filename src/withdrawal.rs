use ethabi::Token;
use std::rc::Rc;
use tiny_keccak::keccak256;
use web3::contract;
use web3::contract::tokens::Detokenize;
use web3::contract::Options;
use web3::futures::prelude::*;
use web3::futures::try_ready;
use web3::types::{Address, BlockNumber, H256, U256};
use web3::DuplexTransport;

use super::relay::Network;
use super::transfer::Transfer;

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
pub struct WithdrawalApprovals {
    approvers: Vec<Address>,
}

impl Detokenize for WithdrawalApprovals {
    /// Creates a new instance from parsed ABI tokens.
    fn from_tokens(tokens: Vec<Token>) -> Result<Self, contract::Error>
    where
        Self: Sized,
    {
        let approval_tokens = tokens[0].clone().to_array().ok_or_else(|| {
            contract::Error::from_kind(contract::ErrorKind::Msg(
                "cannot parse destination address from contract response".to_string(),
            ))
        })?;
        let approvers = approval_tokens
            .iter()
            .filter_map(move |token| token.clone().to_address())
            .collect();
        Ok(WithdrawalApprovals { approvers })
    }
}

pub enum CheckWithdrawalState {
    GetWithdrawal(Box<Future<Item = Withdrawal, Error = ()>>),
    GetWithdrawalApprovals(Box<Future<Item = WithdrawalApprovals, Error = ()>>),
    RequiresApproval(bool),
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
        let next = match self.state {
            CheckWithdrawalState::GetWithdrawal(ref mut future) => {
                let withdrawal = try_ready!(future.poll());
                if (withdrawal.destination == Address::zero() && withdrawal.amount.as_u64() == 0)
                    || (withdrawal.destination == self.transfer.destination
                        && withdrawal.amount == self.transfer.amount)
                {
                    if withdrawal.processed {
                        CheckWithdrawalState::RequiresApproval(false)
                    } else {
                        let target = self.target.clone();
                        let account = target.account;
                        let approval_hash = Self::get_withdrawal_hash(&self.transfer);
                        let approval_future = Box::new(
                            target
                                .relay
                                .query::<WithdrawalApprovals, Address, BlockNumber, H256>(
                                    "withdrawalApprovals",
                                    approval_hash,
                                    account,
                                    Options::default(),
                                    BlockNumber::Latest,
                                )
                                .map_err(|e| {
                                    error!("error getting withdrawal: {:?}", e);
                                }),
                        );
                        CheckWithdrawalState::GetWithdrawalApprovals(approval_future)
                    }
                } else {
                    error!("Withdrawal from contractt did not match transfer");
                    CheckWithdrawalState::RequiresApproval(false)
                }
            }
            CheckWithdrawalState::GetWithdrawalApprovals(ref mut future) => {
                let withdrawal_approvals = try_ready!(future.poll());
                let account = self.target.account;
                withdrawal_approvals.approvers.iter().for_each(|approval| {
                    info!("Found approver: {}", approval);
                });
                CheckWithdrawalState::RequiresApproval(!withdrawal_approvals.approvers.contains(&account))
            }
            CheckWithdrawalState::RequiresApproval(b) => CheckWithdrawalState::RequiresApproval(b),
        };
        if let CheckWithdrawalState::RequiresApproval(b) = next {
            return Ok(Async::Ready(b));
        }
        self.state = next;
        Ok(Async::NotReady)
    }
}
