use ethabi::Token;
use tiny_keccak::keccak256;
use web3::contract;
use web3::contract::tokens::{Detokenize, Tokenize};
use web3::types::{Address, H256, U256};

use transfers::transfer::Transfer;

/// Fees struct for taking the FeeQuery and parsing the Tokens
#[derive(Debug, Clone)]
pub struct Fees(pub U256);

impl Detokenize for Fees {
    /// Creates a new instance from parsed ABI tokens.
    fn from_tokens(tokens: Vec<Token>) -> Result<Self, contract::Error>
    where
        Self: Sized,
    {
        let fee = tokens[0].clone().to_uint().ok_or_else(|| {
            contract::Error::from_kind(contract::ErrorKind::Msg(
                "cannot parse fees from contract response".to_string(),
            ))
        })?;
        debug!("fees: {:?}", fee);
        Ok(Fees(fee))
    }
}

/// Query with args for getting the fees
#[derive(Default)]
pub struct FeeQuery {}

impl Tokenize for FeeQuery {
    fn into_tokens(self) -> Vec<Token> {
        vec![]
    }
}

/// struct for parsing tokens from getting a Withdrawal
#[derive(Debug, Clone)]
pub struct Withdrawal {
    pub destination: Address,
    pub amount: U256,
    pub processed: bool,
}

impl Withdrawal {
    pub fn get_withdrawal_hash(transfer: &Transfer) -> H256 {
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

/// Deconstruct withdrawal approval values from contract into usabale values
#[derive(Debug, Clone)]
pub struct WithdrawalApprovals(pub Address);

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

/// Query Args for finding approvals
pub struct WithdrawalApprovalQuery {
    approval_hash: H256,
    index: U256,
}

impl WithdrawalApprovalQuery {
    pub fn new(approval_hash: &H256, index: &U256) -> Self {
        WithdrawalApprovalQuery {
            approval_hash: *approval_hash,
            index: *index,
        }
    }
}

impl Tokenize for WithdrawalApprovalQuery {
    fn into_tokens(self) -> Vec<Token> {
        vec![Token::FixedBytes(self.approval_hash.to_vec()), Token::Uint(self.index)]
    }
}
