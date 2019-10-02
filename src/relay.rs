use failure::{Error, SyncFailure};
use lru::LruCache;
use std::rc::Rc;
use std::sync::atomic::AtomicUsize;
use std::sync::RwLock;
use std::{process, time};
use tokio_core::reactor;
use web3::confirm::{wait_for_transaction_confirmation, SendTransactionWithConfirmation};
use web3::contract::Contract;
use web3::futures::future::Either;
use web3::futures::sync::mpsc;
use web3::futures::Future;
use web3::types::{Address, FilterBuilder, TransactionReceipt, H256, U256};
use web3::{DuplexTransport, Web3};

use super::anchors::anchor::HandleAnchors;
use super::errors::OperationError;
use super::eth::contracts::{FLUSH_EVENT_SIGNATURE, TRANSFER_EVENT_SIGNATURE};
use super::eth::utils::clean_0x;
use super::extensions::removed::{CancelRemoved, ExitOnLogRemoved};
use super::server::{HandleRequests, RequestType};
use super::transfers::flush::ProcessFlush;
use super::transfers::live::ProcessTransfer;
use super::transfers::live::WatchLiveLogs;
use super::transfers::past::RecheckPastTransferLogs;

const FREE_GAS_PRICE: u64 = 0;
const GAS_LIMIT: u64 = 200_000;

/// Add CheckRemoved trait to SendTransactionWithConfirmation, which is called by Transfer::approve_withdrawal
impl<T> CancelRemoved<T, TransactionReceipt, web3::Error> for SendTransactionWithConfirmation<T>
where
    T: DuplexTransport + 'static,
{
    fn cancel_removed(
        self,
        target: &Network<T>,
        tx_hash: H256,
    ) -> ExitOnLogRemoved<T, TransactionReceipt, web3::Error> {
        ExitOnLogRemoved::new(target, tx_hash, Box::new(self))
    }
}

/// Token relay between two Ethereum networks
pub struct Relay<T: DuplexTransport + 'static> {
    homechain: Network<T>,
    sidechain: Network<T>,
}

impl<T: DuplexTransport + 'static> Relay<T> {
    /// Constructs a token relay given two Ethereum networks
    ///
    /// # Arguments
    ///
    /// * `homechain` - Network to be used as the home chain
    /// * `sidechain` - Network to be used as the side chain
    pub fn new(homechain: Network<T>, sidechain: Network<T>) -> Self {
        Self { homechain, sidechain }
    }

    fn handle_requests(&self, rx: mpsc::UnboundedReceiver<RequestType>, handle: &reactor::Handle) -> HandleRequests<T> {
        HandleRequests::new(&self.homechain, &self.sidechain, rx, handle)
    }

    pub fn unlock(&self, password: &str) -> impl Future<Item = (), Error = Error> {
        self.homechain
            .unlock(password)
            .join(self.sidechain.unlock(password))
            .and_then(|_| Ok(()))
    }

    /// Returns a Future representing the operation of the token relay, including forwarding
    /// Transfer events and anchoring sidechain blocks onto the homechain
    ///
    /// # Arguments
    ///
    /// * `handle` - Handle to the event loop to spawn additional futures
    pub fn run(
        &self,
        rx: mpsc::UnboundedReceiver<RequestType>,
        handle: &reactor::Handle,
    ) -> impl Future<Item = (), Error = ()> {
        self.sidechain
            .handle_anchors(&self.homechain, handle)
            .join(self.homechain.watch_transfer_logs(&self.sidechain, handle))
            .join(self.sidechain.watch_transfer_logs(&self.homechain, handle))
            .join(self.homechain.recheck_past_transfer_logs(&self.sidechain, handle))
            .join(self.sidechain.recheck_past_transfer_logs(&self.homechain, handle))
            .join(self.sidechain.watch_flush_logs(&self.homechain, handle))
            .join(self.handle_requests(rx, handle))
            .and_then(|_| Ok(()))
            .map_err(|_| {
                process::exit(-1);
            })
    }
}

#[derive(Clone, Debug, Copy, PartialEq, Eq)]
pub enum TransferApprovalState {
    Sent,
    Removed,
}

/// Networks are considered either the homechain or the sidechain for the purposes of relaying
/// token transfers
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum NetworkType {
    /// The homechain
    Home,
    /// The sidechain
    Side,
}

/// Represents an Ethereum network with a deployed ERC20Relay contract
#[derive(Clone)]
pub struct Network<T: DuplexTransport + 'static> {
    pub network_type: NetworkType,
    pub web3: Web3<T>,
    pub account: Address,
    pub token: Rc<Contract<T>>,
    pub relay: Rc<Contract<T>>,
    pub free: bool,
    pub confirmations: u64,
    pub anchor_frequency: u64,
    pub interval: u64,
    pub timeout: u64,
    pub chain_id: u64,
    pub keydir: String,
    pub password: String,
    pub nonce: Rc<AtomicUsize>,
    pub pending: Rc<RwLock<LruCache<H256, TransferApprovalState>>>,
    pub retries: u64,
}

impl<T: DuplexTransport + 'static> Network<T> {
    /// Constructs a new network
    ///
    /// # Arguments
    ///
    /// * `network_type` - The type of the network (homechain or sidechain)
    /// * `transport` - The transport to use for interacting with the network
    /// * `token` - Address of the ERC20 token contract to use
    /// * `relay` - Address of the ERC20Relay contract to use
    /// * `confirmations` - Number of blocks to wait for confirmation
    /// * `anchor_frequency` - Frequency of sidechain anchor blocks
    /// * `interval` - Number of seconds between each lookback attempt
    pub fn new(
        network_type: NetworkType,
        transport: T,
        account: &str,
        token: &str,
        token_abi: &str,
        relay: &str,
        relay_abi: &str,
        free: bool,
        confirmations: u64,
        anchor_frequency: u64,
        interval: u64,
        timeout: u64,
        chain_id: u64,
        keydir: &str,
        password: &str,
        nonce: AtomicUsize,
        retries: u64,
    ) -> Result<Self, OperationError> {
        let web3 = Web3::new(transport);
        let account = clean_0x(account)
            .parse()
            .or_else(|_| Err(OperationError::InvalidAddress(account.into())))?;

        let token_address: Address = clean_0x(token)
            .parse()
            .or_else(|_| Err(OperationError::InvalidAddress(token.into())))?;

        let relay_address: Address = clean_0x(relay)
            .parse()
            .or_else(|_| Err(OperationError::InvalidAddress(relay.into())))?;

        let token = Rc::new(
            Contract::from_json(web3.eth(), token_address, token_abi.as_bytes())
                .or(Err(OperationError::InvalidContractAbi))?,
        );

        let relay = Rc::new(
            Contract::from_json(web3.eth(), relay_address, relay_abi.as_bytes())
                .or(Err(OperationError::InvalidContractAbi))?,
        );

        Ok(Self {
            network_type,
            web3,
            account,
            token,
            relay,
            free,
            confirmations,
            anchor_frequency,
            interval,
            timeout,
            chain_id,
            keydir: keydir.to_string(),
            password: password.to_string(),
            nonce: Rc::new(nonce),
            pending: Rc::new(RwLock::new(LruCache::new(4096))),
            retries,
        })
    }

    /// Constructs a new home network
    ///
    /// # Arguments
    ///
    /// * `transport` - The transport to use for interacting with the network
    /// * `token` - Address of the ERC20 token contract to use
    /// * `relay` - Address of the ERC20Relay contract to use
    /// * `confirmations` - Number of blocks to wait for confirmation
    pub fn homechain(
        transport: T,
        account: &str,
        token: &str,
        token_abi: &str,
        relay: &str,
        relay_abi: &str,
        free: bool,
        confirmations: u64,
        interval: u64,
        timeout: u64,
        chain_id: u64,
        keydir: &str,
        password: &str,
        nonce: AtomicUsize,
        retries: u64,
    ) -> Result<Self, OperationError> {
        Self::new(
            NetworkType::Home,
            transport,
            account,
            token,
            token_abi,
            relay,
            relay_abi,
            free,
            confirmations,
            0,
            interval,
            timeout,
            chain_id,
            keydir,
            password,
            nonce,
            retries,
        )
    }

    /// Constructs a new side network
    ///
    /// # Arguments
    ///
    /// * `transport` - The transport to use for interacting with the network
    /// * `token` - Address of the ERC20 token contract to use
    /// * `relay` - Address of the ERC20Relay contract to use
    /// * `confirmations` - Number of blocks to wait for confirmation
    /// * `anchor_frequency` - Frequency of sidechain anchor blocks
    pub fn sidechain(
        transport: T,
        account: &str,
        token: &str,
        token_abi: &str,
        relay: &str,
        relay_abi: &str,
        free: bool,
        confirmations: u64,
        anchor_frequency: u64,
        interval: u64,
        timeout: u64,
        chain_id: u64,
        keydir: &str,
        password: &str,
        nonce: AtomicUsize,
        retries: u64,
    ) -> Result<Self, OperationError> {
        Self::new(
            NetworkType::Side,
            transport,
            account,
            token,
            token_abi,
            relay,
            relay_abi,
            free,
            confirmations,
            anchor_frequency,
            interval,
            timeout,
            chain_id,
            keydir,
            password,
            nonce,
            retries,
        )
    }

    /// Unlock an account with a password
    ///
    /// # Arguments
    ///
    /// * `password` - Password for the account's keystore
    pub fn unlock(&self, password: &str) -> impl Future<Item = (), Error = Error> {
        let account = self.account;
        self.web3
            .personal()
            .unlock_account(account, password, Some(0))
            .map_err(SyncFailure::new)
            .map_err(|e| e.into())
            .and_then(move |success| {
                if !success {
                    return Err(OperationError::CouldNotUnlockAccount(format!("{:?}", &account)))?;
                }
                Ok(())
            })
    }

    /// Returns a WatchFlush Future for this chain.
    /// Watches the self (which should only be sidechain), and sends to the target, which should be
    /// homechain
    ///
    /// # Arguments
    ///
    /// * `target` - Network where to anchor the block headers
    /// * `handle` - Handle to spawn new tasks
    pub fn watch_flush_logs(&self, target: &Network<T>, handle: &reactor::Handle) -> ProcessFlush<T> {
        let (tx, rx) = mpsc::unbounded();
        let filter = FilterBuilder::default()
            .address(vec![self.relay.address()])
            .topics(Some(vec![FLUSH_EVENT_SIGNATURE.into()]), None, None, None)
            .build();
        // This on just watches right inside, no tx to send to
        let watch = WatchLiveLogs::new(self, target, &filter, &tx, handle)
            .map_err(move |e| error!("error watching transaction logs {:?}", e));
        // We do this in a separately spawned task because we have to wait 20 blocks per
        handle.spawn(watch);
        ProcessFlush::new(self, target, rx)
    }

    /// Returns a ProcessTransfer Future for this chain.
    /// Watches the self network, and sends transactions to the target network
    ///
    /// # Arguments
    ///
    /// * `target` - Network where to anchor the block headers
    /// * `handle` - Handle to spawn new tasks
    pub fn watch_transfer_logs(&self, target: &Network<T>, handle: &reactor::Handle) -> ProcessTransfer<T> {
        let (tx, rx) = mpsc::unbounded();
        let filter = FilterBuilder::default()
            .address(vec![self.token.address()])
            .topics(
                Some(vec![TRANSFER_EVENT_SIGNATURE.into()]),
                None,
                Some(vec![self.relay.address().into()]),
                None,
            )
            .build();
        let watch = WatchLiveLogs::new(self, target, &filter, &tx, handle)
            .map_err(move |e| error!("error watching transaction logs {:?}", e));
        // We do this in a separately spawned task because we have to wait 20 blocks per
        handle.spawn(watch);
        ProcessTransfer::new(self, target, rx, handle)
    }

    /// Returns a RecheckPastTransferLogs Future for this chain.
    /// Will approve Transfers found in this network, to the target network.
    ///
    /// # Arguments
    ///
    /// * `target` - Network where to anchor the block headers
    /// * `handle` - Handle to spawn new tasks
    pub fn recheck_past_transfer_logs(&self, target: &Network<T>, handle: &reactor::Handle) -> RecheckPastTransferLogs {
        RecheckPastTransferLogs::new(self, target, handle)
    }

    /// Returns a HandleAnchors Future for this chain.
    /// Will anchor block headers from this network to the target network.
    ///
    /// # Arguments
    ///
    /// * `target` - Network where to anchor the block headers
    /// * `handle` - Handle to spawn new tasks
    pub fn handle_anchors(&self, target: &Network<T>, handle: &reactor::Handle) -> HandleAnchors<T> {
        HandleAnchors::new(self, target, handle)
    }

    /// Returns the gas limit for the network as a U256
    pub fn get_gas_limit(&self) -> U256 {
        GAS_LIMIT.into()
    }

    /// Takes in an estimated price, per the server.
    /// Change gas price to 0 if free is set in config
    pub fn finalize_gas_price(&self, potential_gas_price: U256) -> U256 {
        if self.free {
            return FREE_GAS_PRICE.into();
        }
        potential_gas_price
    }

    ///Returns a transaction receipt after waiting if not removed
    ///The state of the transaction is stored on the target chain
    pub fn get_receipt(
        &self,
        removed: bool,
        transaction_hash: H256,
    ) -> Box<Future<Item = Option<TransactionReceipt>, Error = ()>> {
        let source = self.clone();
        let web3 = self.web3.clone();
        let network_type = self.network_type;
        let confirmations = self.confirmations as usize;
        let transport = web3.transport().clone();
        let future = if removed {
            Either::A(web3.eth().transaction_receipt(transaction_hash))
        } else {
            info!(
                "received transfer event in tx hash {:?} on {:?}, waiting for confirmations",
                &transaction_hash, network_type
            );
            Either::B(
                wait_for_transaction_confirmation(
                    transport,
                    transaction_hash,
                    time::Duration::from_secs(1),
                    confirmations,
                )
                .cancel_removed(&source, transaction_hash),
            )
        }
        .map_err(move |_| {
            error!("error checking transaction on {:?}", network_type);
            ()
        });
        Box::new(future)
    }
}
