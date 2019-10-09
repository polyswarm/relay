use ethabi::Token;
use std::string::ToString;
use web3::contract;
use web3::contract::tokens::{Detokenize, Tokenize};
use web3::types::{Address, U256};

/// Fee Wallet struct for taking the FeeWalletQuery and parsing the Tokens
#[derive(Debug, Clone)]
pub struct FeeWallet(pub Address);

impl Detokenize for FeeWallet {
    /// Creates a new instance from parsed ABI tokens.
    fn from_tokens(tokens: Vec<Token>) -> Result<Self, contract::Error>
    where
        Self: Sized,
    {
        let fee_wallet = tokens[0].clone().to_address().ok_or_else(|| {
            contract::Error::from_kind(contract::ErrorKind::Msg(
                "cannot parse fee wallet from contract response".to_string(),
            ))
        })?;
        debug!("fees wallet: {:?}", fee_wallet);
        Ok(FeeWallet(fee_wallet))
    }
}

/// Query with args for getting the fee wallet
#[derive(Default)]
pub struct FeeWalletQuery();

impl Tokenize for FeeWalletQuery {
    fn into_tokens(self) -> Vec<Token> {
        vec![]
    }
}

/// struct for taking the FlushBlockQuery  and parsing the Tokens
#[derive(Debug, Clone)]
pub struct FlushBlock(pub U256);

impl Detokenize for FlushBlock {
    /// Creates a new instance from parsed ABI tokens.
    fn from_tokens(tokens: Vec<Token>) -> Result<Self, contract::Error>
    where
        Self: Sized,
    {
        let block = tokens[0].clone().to_uint().ok_or_else(|| {
            contract::Error::from_kind(contract::ErrorKind::Msg(
                "cannot parse flush blockfrom contract response".to_string(),
            ))
        })?;
        debug!("flush block: {:?}", block);
        Ok(FlushBlock(block))
    }
}

/// Query for getting the current FlushBlock from the relay contract
#[derive(Default)]
pub struct FlushBlockQuery();

impl Tokenize for FlushBlockQuery {
    fn into_tokens(self) -> Vec<Token> {
        vec![]
    }
}
