use ethabi::Token;
use web3::contract::tokens::Tokenize;
use web3::types::{Address, H256, U256, U64};

use transfers::transfer::Transfer;

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
    pub block_number: U64,
}

impl Tokenize for ApproveParams {
    fn into_tokens(self) -> Vec<Token> {
        let mut tokens = Vec::new();
        tokens.push(Token::Address(self.destination));
        tokens.push(Token::Uint(self.amount));
        tokens.push(Token::FixedBytes(self.tx_hash[..].to_vec()));
        tokens.push(Token::FixedBytes(self.block_hash[..].to_vec()));
        tokens.push(Token::Uint(self.block_number.as_u64().into()));
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
    pub block_number: U64,
}

impl Tokenize for UnapproveParams {
    fn into_tokens(self) -> Vec<Token> {
        let mut tokens = Vec::new();
        tokens.push(Token::FixedBytes(self.tx_hash[..].to_vec()));
        tokens.push(Token::FixedBytes(self.block_hash[..].to_vec()));
        tokens.push(Token::Uint(self.block_number.as_u64().into()));
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
