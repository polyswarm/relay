use std::fmt;
use tiny_keccak::keccak256;
use web3::contract::tokens::Tokenize;
use web3::futures::future::Future;
use web3::types::{Address, TransactionReceipt, H256, U256, U64};
use web3::DuplexTransport;

use crate::eth::transaction::SendTransaction;
use crate::extensions::removed::{CancelRemoved, ExitOnLogRemoved};
use crate::relay::Network;
use crate::transfers::withdrawal::{ApproveWithdrawal, DoesRequireApproval, UnapproveParams};

/// Add CheckRemoved trait to SendTransaction, which is called by Transfer::approve_withdrawal
impl<T, P> CancelRemoved<T, (), ()> for SendTransaction<T, P>
where
    T: DuplexTransport + 'static,
    P: Tokenize + Clone + 'static,
{
    fn cancel_removed(self, target: &Network<T>, tx_hash: H256) -> ExitOnLogRemoved<T, (), ()> {
        ExitOnLogRemoved::new(target, tx_hash, Box::new(self))
    }
}

/// Represents a token transfer between two networks
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct Transfer {
    pub destination: Address,
    pub amount: U256,
    pub tx_hash: H256,
    pub block_hash: H256,
    pub block_number: U64,
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
    pub fn check_withdrawal<T: DuplexTransport + 'static>(
        &self,
        target: &Network<T>,
        fees: Option<U256>,
    ) -> DoesRequireApproval<T> {
        DoesRequireApproval::new(target, self, fees)
    }

    /// Returns a Future that will transaction with "approve_withdrawal" on the ERC20Relay contract
    ///
    /// # Arguments
    ///
    /// * `target` - Network where the withdrawals is performed
    pub fn approve_withdrawal<T: DuplexTransport + 'static>(
        &self,
        source: &Network<T>,
        target: &Network<T>,
    ) -> ApproveWithdrawal<T> {
        ApproveWithdrawal::new(source, target, self)
    }

    pub fn unapprove_withdrawal<T: DuplexTransport + 'static>(
        &self,
        target: &Network<T>,
    ) -> impl Future<Item = (), Error = ()> {
        info!("unapproving withdrawal on {:?}: {} ", target.network_type, self);
        SendTransaction::new(
            target,
            "unapproveWithdrawal",
            &UnapproveParams::from(*self),
            target.retries,
        )
        .or_else(|_| Ok(()))
    }

    pub fn get_withdrawal_hash(&self) -> H256 {
        let tx_hash = self.tx_hash;
        let block_hash = self.block_hash;
        let block_number: &mut [u8] = &mut [0; 32];
        // Must be 256 bit version of block number to match the hash
        let block_number_256: U256 = self.block_number.as_u64().into();
        block_number_256.to_big_endian(block_number);
        let mut grouped: Vec<u8> = Vec::new();
        grouped.extend_from_slice(&tx_hash.0);
        grouped.extend_from_slice(&block_hash.0);
        grouped.extend_from_slice(block_number);
        H256(keccak256(&grouped[..]))
    }
}

impl fmt::Display for Transfer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "({} → {:?}, hash: {:?})",
            self.amount, self.destination, self.tx_hash
        )
    }
}
