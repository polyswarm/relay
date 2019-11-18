use common_types::transaction::{Action, Transaction as RawTransactionRequest, UnverifiedTransaction};
use ethstore::accounts_dir::RootDiskDirectory;
use ethstore::{EthStore, SimpleSecretStore, StoreAccountRef};
use rlp::Encodable;
use rlp::RlpStream;
use std::sync::atomic::Ordering;
use web3::contract::tokens::Tokenize;
use web3::futures::prelude::*;
use web3::futures::try_ready;
use web3::types::{TransactionReceipt, U256};
use web3::DuplexTransport;

use crate::errors::OperationError;
use crate::relay::Network;

pub enum TransactionState<T, P>
where
    T: DuplexTransport + 'static,
    P: Tokenize + Clone,
{
    Build(BuildTransaction<T, P>),
    Send(Box<dyn Future<Item = TransactionReceipt, Error = web3::error::Error>>),
    ResyncNonce(Box<dyn Future<Item = U256, Error = ()>>),
}

/// This struct implements Future so that it is easy to build a transaction, with the proper gas price in a future
pub struct BuildTransaction<T, P>
where
    T: DuplexTransport + 'static,
    P: Tokenize + Clone,
{
    target: Network<T>,
    function: String,
    params: P,
    nonce: Option<U256>,
    gas_future: Box<dyn Future<Item = U256, Error = ()>>,
}

impl<T, P> BuildTransaction<T, P>
where
    T: DuplexTransport + 'static,
    P: Tokenize + Clone,
{
    pub fn new(target: &Network<T>, function_name: &str, params: P, nonce: Option<U256>) -> Self {
        let network_type = target.network_type;
        let gas_future = target.web3.eth().gas_price().map_err(move |e| {
            error!("error fetching current gas price on {:?}: {}", network_type, e);
        });

        BuildTransaction {
            target: target.clone(),
            function: function_name.to_string(),
            params,
            nonce,
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
            .sign(
                &StoreAccountRef::root(self.target.account.0.into()),
                &password.into(),
                &raw_tx,
            )
            .map_err(move |e| {
                error!("error signing transaction: {}", e);
                OperationError::CouldNotBuildTransaction("Could not sign transaction".to_string())
            })?;
        let tx_with_sig: UnverifiedTransaction =
            transaction_request.with_signature(signed_tx, Some(self.target.chain_id));
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
        if let Some(nonce) = self.nonce {
            self.target.nonce.store(nonce.as_u64() as usize, Ordering::SeqCst);
        }
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
    target: Network<T>,
    state: TransactionState<T, P>,
    params: P,
    retries: u64, // amount of times relay should try to resync nonce
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
    pub fn new(target: &Network<T>, function: &str, params: &P, retries: u64) -> Self {
        let target = target.clone();
        let future = BuildTransaction::new(&target, function, params.clone(), None);
        let state = TransactionState::Build(future);

        SendTransaction {
            function: function.to_string(),
            target,
            state,
            params: params.clone(),
            retries,
        }
    }
}

impl<T, P: 'static> Future for SendTransaction<T, P>
where
    T: DuplexTransport + 'static,
    P: Tokenize + Clone,
{
    type Item = ();
    type Error = ();
    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        let function = self.function.clone();
        let target = self.target.clone();
        let params = self.params.clone();
        loop {
            let next = match self.state {
                TransactionState::Build(ref mut future) => {
                    let rlp_stream = try_ready!(future.poll());
                    let send_future = self.target.relay.send_raw_call_with_confirmations(
                        rlp_stream.as_raw().into(),
                        self.target.confirmations as usize,
                    );
                    TransactionState::Send(Box::new(send_future))
                }
                TransactionState::Send(ref mut future) => {
                    let target = self.target.clone();
                    let network_type = target.network_type;
                    let function = self.function.clone();
                    match future.poll() {
                        Ok(Async::Ready(receipt)) => {
                            match receipt.status {
                                Some(result) => {
                                    if result == 1.into() {
                                        info!("{} on {:?} successful: {:?}", function, network_type, receipt);
                                    } else {
                                        error!("{} on {:?} failed: {:?}", function, network_type, receipt);
                                        return Err(());
                                    }
                                }
                                None => {
                                    error!(
                                        "{} receipt on {:?} has no status: {:?}",
                                        function, network_type, receipt
                                    );
                                }
                            }
                            return Ok(Async::Ready(()));
                        }
                        Ok(Async::NotReady) => return Ok(Async::NotReady),
                        Err(e) => {
                            let message = format!("{} failed: {:?}", function, e);
                            if self.retries > 0 && message.contains("nonce too low") {
                                info!(
                                    "Nonce desync detected on {:?}, resyncing nonce and retrying",
                                    network_type
                                );
                                let nonce_future =
                                    target
                                        .web3
                                        .eth()
                                        .transaction_count(target.account, None)
                                        .map_err(move |_e| {
                                            error!("Error getting transaction count on {:?}", network_type);
                                        });
                                TransactionState::ResyncNonce(Box::new(nonce_future))
                            } else {
                                error!("error sending transaction on {:?}: {:?}", network_type, e);
                                return Err(());
                            }
                        }
                    }
                }
                TransactionState::ResyncNonce(ref mut future) => {
                    let nonce = try_ready!(future.poll());
                    let function = function.clone();
                    let params = params.clone();
                    let future = BuildTransaction::new(&target, &function, params, Some(nonce));
                    TransactionState::Build(future)
                }
            };
            self.state = next;
            if let TransactionState::Build(_) = self.state {
                self.retries -= 1;
            }
        }
    }
}
