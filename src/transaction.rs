use ethcore_transaction::{Action, Transaction as RawTransactionRequest};
use ethstore::accounts_dir::RootDiskDirectory;
use ethstore::{EthStore, SimpleSecretStore, StoreAccountRef};
use rlp::{Encodable, RlpStream};
use std::rc::Rc;
use std::sync::atomic::Ordering;
use web3::contract::tokens::Tokenize;
use web3::futures::prelude::*;
use web3::futures::try_ready;
use web3::types::{TransactionReceipt, U256};
use web3::DuplexTransport;

use super::errors::OperationError;
use super::relay::Network;

pub enum TransactionState<T, P>
where
    T: DuplexTransport + 'static,
    P: Tokenize + Clone,
{
    Build(BuildTransaction<T, P>),
    Send(Box<Future<Item = TransactionReceipt, Error = ()>>),
}

/// This struct implements Future so that it is easy to build a transaction, with the proper gas price in a future
pub struct BuildTransaction<T, P>
where
    T: DuplexTransport + 'static,
    P: Tokenize + Clone,
{
    target: Rc<Network<T>>,
    function: String,
    params: P,
    gas_future: Box<Future<Item = U256, Error = ()>>,
}

impl<T, P> BuildTransaction<T, P>
where
    T: DuplexTransport + 'static,
    P: Tokenize + Clone,
{
    pub fn new(target: &Rc<Network<T>>, function_name: &str, params: P) -> Self {
        let gas_future = target.web3.eth().gas_price().map_err(move |e| {
            error!("error fetching current gas price: {}", e);
        });

        BuildTransaction {
            target: target.clone(),
            function: function_name.to_string(),
            params,
            gas_future: Box::new(gas_future),
        }
    }

    /// Mutates a RlpStream with ready to send transaction data
    ///
    /// # Arguments
    ///
    /// * `input_data` - Function data from the contract this transaction is being built for
    /// * `gas` - The gas limit for this transaction
    /// * `gas_price` - The gas price for this transaction
    /// * `value` - The eth value transferred in this transaction
    /// * `nonce` - Transaction nonce
    pub fn build_transaction(
        &self,
        input_data: &[u8],
        gas: U256,
        gas_price: U256,
        value: U256,
        nonce: U256,
    ) -> Result<RlpStream, OperationError> {
        let store = self.get_store_for_keyfiles();
        let transaction_request = RawTransactionRequest {
            action: Action::Call(self.target.relay.address()),
            gas,
            gas_price,
            value,
            nonce,
            data: input_data.to_vec(),
        };
        let password = self.target.password.clone();
        let raw_tx = transaction_request.hash(Some(self.target.chain_id));
        let signed_tx = store
            .sign(&StoreAccountRef::root(self.target.account), &password.into(), &raw_tx)
            .map_err(move |e| {
                error!("error signing transaction: {}", e);
                OperationError::CouldNotBuildTransaction("Could not sign transaction".to_string())
            })?;
        let tx_with_sig = transaction_request.with_signature(signed_tx, Some(self.target.chain_id));
        let mut stream = RlpStream::new();
        tx_with_sig.rlp_append(&mut stream);
        Ok(stream)
    }

    /// Returns a keyfile store of accounts to be used for signing
    ///
    /// # Arguments
    ///
    /// * `keyfile_dir` - directory of keyfiles
    pub fn get_store_for_keyfiles(&self) -> EthStore {
        let keyfile_dir = self.target.keydir.clone();
        let path = match ::std::fs::metadata(keyfile_dir.clone()) {
            Ok(_) => &keyfile_dir,
            Err(_) => "",
        };
        let dir = RootDiskDirectory::at(path);
        EthStore::open(Box::new(dir)).unwrap()
    }
}

impl<T, P> Future for BuildTransaction<T, P>
where
    T: DuplexTransport + 'static,
    P: Tokenize + Clone,
{
    type Item = RlpStream;
    type Error = ();
    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        // Get gas price
        let eth_gas_price = try_ready!(self.gas_future.poll());
        // create input data
        let params = self.params.clone();
        let function = self.function.clone();
        let input_data = self
            .target
            .relay
            .get_function_data(&function, params)
            .map_err(move |e| {
                error!("error build input data: {}", e);
            })?;
        let gas = self.target.get_gas_limit();
        let gas_price = self.target.finalize_gas_price(eth_gas_price);
        let nonce = U256::from(self.target.nonce.load(Ordering::SeqCst));
        self.target.nonce.fetch_add(1, Ordering::SeqCst);
        match self.build_transaction(&input_data, gas, gas_price, 0.into(), nonce) {
            Ok(stream) => Ok(Async::Ready(stream)),
            Err(e) => {
                error!("error building transaction: {}", e);
                Err(())
            }
        }
    }
}

/// Future that calls the ERC20RelayContract to approve a transfer across the relay
pub struct SendTransaction<T, P>
where
    T: DuplexTransport + 'static,
    P: Tokenize + Clone,
{
    function: String,
    target: Rc<Network<T>>,
    state: TransactionState<T, P>,
}

impl<T, P> SendTransaction<T, P>
where
    T: DuplexTransport + 'static,
    P: Tokenize + Clone,
{
    /// Returns a newly created ApproveWithdrawal Future
    ///
    /// # Arguments
    ///
    /// * `target` - Network where the withdrawal will be posted to the contract
    /// * `function` - Name of the function to call
    /// * `params` - Vec of Tokens corresponsind to the params for the contract function parameters
    pub fn new(target: &Rc<Network<T>>, function: &str, params: &P) -> Self {
        let target = target.clone();
        let future = BuildTransaction::new(&target, function, params.clone());
        let state = TransactionState::Build(future);
        SendTransaction {
            function: function.to_string(),
            target,
            state,
        }
    }
}

impl<T, P> Future for SendTransaction<T, P>
where
    T: DuplexTransport + 'static,
    P: Tokenize + Clone,
{
    type Item = ();
    type Error = ();
    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        let function = self.function.clone();
        loop {
            let next = match self.state {
                TransactionState::Build(ref mut future) => {
                    let function = function.clone();
                    let rlp_stream = try_ready!(future.poll());
                    let send_future = self
                        .target
                        .relay
                        .send_raw_call_with_confirmations(
                            rlp_stream.as_raw().into(),
                            self.target.confirmations as usize,
                        )
                        .map_err(move |e| {
                            error!("error completing {} transaction: {}", function, e);
                        });
                    TransactionState::Send(Box::new(send_future))
                }
                TransactionState::Send(ref mut future) => {
                    let receipt = try_ready!(future.poll());
                    match receipt.status {
                        Some(result) => {
                            if result == 1.into() {
                                info!("{} successful: {:?}", function, receipt);
                            } else {
                                warn!("{} failed: {:?}", function, receipt);
                            }
                        }
                        None => {
                            error!("{} receipt has no status: {:?}", function, receipt);
                        }
                    }
                    return Ok(Async::Ready(()));
                }
            };
            self.state = next;
        }
    }
}
