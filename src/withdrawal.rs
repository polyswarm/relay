use std::rc::Rc;
use tiny_keccak::keccak256;
use ethabi::Token;
use web3::contract::tokens::Detokenize;
use web3::contract;
use web3::futures::prelude::*;
use web3::futures::try_ready;
use web3::types::{Address, BlockNumber, H256, U256};
use web3::DuplexTransport;
use web3::contract::Options;

use super::relay::{Transfer, Network};

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

/// Returns a Future to retrieve the withdrawal event for this transaction
///
/// # Arguments
///
/// * `transfer` - A Transfer struct with the tx_hash, block_hash, and block_number we need to retrieve the withdrawal
pub struct GetWithdrawalFuture {
    transfer: Transfer,
    future: Box<Future<Item = Withdrawal, Error = ()>>,
}

impl GetWithdrawalFuture {
    pub fn new<T: DuplexTransport + 'static>(network: &Rc<Network<T>>, transfer: Transfer) -> Self {
        let hash = Self::get_withdrawal_hash(&transfer);
        let account = network.account;
        let future = Box::new(network.relay
            .query::<Withdrawal, Address, BlockNumber, H256>(
                "withdrawals",
                hash,
                account,
                Options::default(),
                BlockNumber::Latest,
            ).map_err(|e| {
                error!("error getting withdrawal: {:?}", e);
            }));
        GetWithdrawalFuture {
            transfer,
            future,
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

impl Future for GetWithdrawalFuture {
    /// The type of the value returned when the future completes.
    type Item = Withdrawal;

    /// The type representing errors that occurred while processing the computation.
    type Error = ();

    /// The function that will be repeatedly called to see if the future is
    /// has completed or not
    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        let withdrawal = try_ready!(self.future.poll());
        if withdrawal.destination == Address::zero() && withdrawal.amount.as_u64() == 0 {
            info!("Found transfer that was never approved");
            Ok(Async::Ready(withdrawal))
        } else if withdrawal.destination == self.transfer.destination && withdrawal.amount == self.transfer.amount {
            Ok(Async::Ready(withdrawal))
        } else {
            // let error = contract::Error::from_kind(contract::ErrorKind::Msg("Withdrawal from contract did not match transfer".to_string()));
            // Err(error)
            error!("Withdrawal from contract did not match transfer");
            Err(())
        }
    }
}
