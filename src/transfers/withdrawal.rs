use ethabi::Token;
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

/// Parameters for the approveWithdrawal function.
///
/// Implements Tokenize so it can be passed to SendTransaction
///
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct ApproveParams {
    pub destination: Address,
    pub amount: U256,
    pub tx_hash: H256,
    pub block_hash: H256,
    pub block_number: U256,
}

impl Tokenize for ApproveParams {
    fn into_tokens(self) -> Vec<Token> {
        let mut tokens = Vec::new();
        tokens.push(Token::Address(self.destination));
        tokens.push(Token::Uint(self.amount));
        tokens.push(Token::FixedBytes(self.tx_hash.to_vec()));
        tokens.push(Token::FixedBytes(self.block_hash.to_vec()));
        tokens.push(Token::Uint(self.block_number));
        tokens
    }
}

impl From<Transfer> for ApproveParams {
    fn from(transfer: Transfer) -> Self {
        ApproveParams {
            destination: transfer.destination,
            amount: transfer.amount,
            tx_hash: transfer.tx_hash,
            block_hash: transfer.block_hash,
            block_number: transfer.block_number,
        }
    }
}

/// Parameters for the unapproveWithdrawal function.
///
/// Implements Tokenize so it can be passed to SendTransaction
///
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct UnapproveParams {
    pub tx_hash: H256,
    pub block_hash: H256,
    pub block_number: U256,
}

impl Tokenize for UnapproveParams {
    fn into_tokens(self) -> Vec<Token> {
        let mut tokens = Vec::new();
        tokens.push(Token::FixedBytes(self.tx_hash.to_vec()));
        tokens.push(Token::FixedBytes(self.block_hash.to_vec()));
        tokens.push(Token::Uint(self.block_number));
        tokens
    }
}

impl From<Transfer> for UnapproveParams {
    fn from(transfer: Transfer) -> Self {
        UnapproveParams {
            tx_hash: transfer.tx_hash,
            block_hash: transfer.block_hash,
            block_number: transfer.block_number,
        }
    }
}

/// Withdrawal event added to contract after a transfer
#[derive(Debug, Clone)]
pub struct Withdrawal {
    pub destination: Address,
    pub amount: U256,
    pub processed: bool,
}

impl Withdrawal {
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
    target: Network<T>,
    transfer: Transfer,
    state: CheckWithdrawalState,
}

impl<T: DuplexTransport + 'static> DoesRequireApproval<T> {
    pub fn new(target: &Network<T>, transfer: &Transfer) -> Self {
        let approval_hash = Withdrawal::get_withdrawal_hash(transfer);
        let account = target.account;
        let network_type = target.network_type;
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
                .map_err(move |e| {
                    error!("error getting withdrawal on {:?}: {:?}", network_type, e);
                }),
        );
        let state = CheckWithdrawalState::GetWithdrawal(future);
        DoesRequireApproval {
            target: target.clone(),
            transfer: *transfer,
            state,
        }
    }
}

impl<T: DuplexTransport + 'static> Future for DoesRequireApproval<T> {
    type Item = bool;
    type Error = ();

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        let network_type = self.target.network_type;
        loop {
            let next = match self.state {
                CheckWithdrawalState::GetWithdrawal(ref mut future) => {
                    let withdrawal = try_ready!(future.poll());
                    if (withdrawal.destination == Address::zero() && withdrawal.amount.as_u64() == 0)
                        || (withdrawal.destination == self.transfer.destination
                            && withdrawal.amount == self.transfer.amount)
                    {
                        info!("found withdrawal on {:?}: {:?}", network_type, withdrawal);
                        if withdrawal.processed {
                            return Ok(Async::Ready(false));
                        } else {
                            debug!("transaction not processed on {:?} - checking approvers", network_type);
                            let approval_hash = Withdrawal::get_withdrawal_hash(&self.transfer);
                            let approval_future = GetWithdrawalApprovals::new(&self.target, &approval_hash, &0.into());
                            CheckWithdrawalState::GetWithdrawalApprovals(0, approval_future)
                        }
                    } else {
                        error!(
                            "withdrawal from contract did not match transfer on {:?}: {:?} {:?}",
                            network_type, self.transfer, withdrawal
                        );
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
                                let approval_hash = Withdrawal::get_withdrawal_hash(&self.transfer);
                                let approval_future =
                                    GetWithdrawalApprovals::new(&self.target, &approval_hash, &i.into());
                                CheckWithdrawalState::GetWithdrawalApprovals(i, approval_future)
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

pub struct GetWithdrawalApprovals(Box<Future<Item = WithdrawalApprovals, Error = ()>>);

impl GetWithdrawalApprovals {
    pub fn new<T: DuplexTransport + 'static>(target: &Network<T>, approval_hash: &H256, index: &U256) -> Self {
        let target = target.clone();
        let account = target.account;
        let network_type = target.network_type;
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
                    info!("found all approvers for {:?} on {:?}", network_type, approval_hash);
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

pub struct WaitForWithdrawalProcessed<T: DuplexTransport + 'static> {
    target: Network<T>,
    transfer: Transfer,
    future: Box<Future<Item = Withdrawal, Error = ()>>,
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

    fn check(target: &Network<T>, transfer: &Transfer) -> Box<Future<Item = Withdrawal, Error = ()>> {
        let approval_hash = Withdrawal::get_withdrawal_hash(transfer);
        let account = target.account;
        let network_type = target.network_type;
        Box::new(
            target
                .relay
                .query::<Withdrawal, Address, BlockNumber, H256>(
                    "withdrawals",
                    approval_hash,
                    account,
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
            if withdrawal.processed {
                return Ok(Async::Ready(()));
            } else {
                self.future = WaitForWithdrawalProcessed::check(&self.target, &self.transfer);
            }
        }
    }
}
