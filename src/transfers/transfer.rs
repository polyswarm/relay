use std::fmt;
use web3::contract::tokens::Tokenize;
use web3::futures::future::{err, ok, Future};
use web3::types::{Address, TransactionReceipt, H256, U256, U64};
use web3::DuplexTransport;

use eth::transaction::SendTransaction;
use extensions::removed::{CancelRemoved, ExitOnLogRemoved};
use relay::Network;
use transfers::withdrawal::{ApproveParams, DoesRequireApproval, UnapproveParams};

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
    ) -> Box<dyn Future<Item = (), Error = ()>> {
        match source.flushed.read() {
            Ok(lock) => {
                if lock.is_some() {
                    warn!(
                        "cannot approve withdrawal after flush on {:?}: {} ",
                        source.network_type, self
                    );
                    Box::new(ok(()))
                } else {
                    info!("approving withdrawal on {:?}: {} ", target.network_type, self);
                    let target = target.clone();
                    Box::new(SendTransaction::new(
                        &target,
                        "approveWithdrawal",
                        &ApproveParams::from(*self),
                        target.retries,
                    )
                        .cancel_removed(&source, self.tx_hash)
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
                        })
                        .or_else(|_| Ok(())))
                }
            }
            Err(e) => {
                error!("error acquiring flush event lock: {:?}", e);
                Box::new(err(()))
            }
        }
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
}

impl fmt::Display for Transfer {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "({} → {:?}, hash: {:?})",
            self.amount, self.destination, self.tx_hash
        )
    }
}
