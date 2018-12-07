use std::rc::Rc;
use tokio_core::reactor;

use web3::contract::Contract;
use web3::futures::Future;
use web3::types::{Address, U256};
use web3::{DuplexTransport, Web3};

use failure::{Error, SyncFailure};

use super::anchor::HandleAnchors;
use super::errors::OperationError;
use super::missed_transfer::HandleMissedTransfers;
use super::transfer::HandleTransfers;

const DEFAULT_GAS_PRICE: u64 = 20_000_000_000;
const FREE_GAS_PRICE: u64 = 0;
const GAS_LIMIT: u64 = 200_000;

// From ethereum_types but not reexported by web3
fn clean_0x(s: &str) -> &str {
    if s.starts_with("0x") {
        &s[2..]
    } else {
        s
    }
}

/// Token relay between two Ethereum networks
#[derive(Clone)]
pub struct Relay<T: DuplexTransport> {
    homechain: Rc<Network<T>>,
    sidechain: Rc<Network<T>>,
}

impl<T: DuplexTransport + 'static> Relay<T> {
    /// Constructs a token relay given two Ethereum networks
    ///
    /// # Arguments
    ///
    /// * `homechain` - Network to be used as the home chain
    /// * `sidechain` - Network to be used as the side chain
    pub fn new(homechain: Network<T>, sidechain: Network<T>) -> Self {
        Self {
            homechain: Rc::new(homechain),
            sidechain: Rc::new(sidechain),
        }
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
    pub fn run(&self, handle: &reactor::Handle) -> impl Future<Item = (), Error = ()> {
        self.sidechain
            .handle_anchors(&self.homechain, handle)
            .join(self.homechain.handle_transfers(&self.sidechain, handle))
            .join(self.sidechain.handle_transfers(&self.homechain, handle))
            .join(self.homechain.handle_missed_transfers(&self.sidechain, handle))
            .join(self.sidechain.handle_missed_transfers(&self.homechain, handle))
            .and_then(|_| Ok(()))
    }
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
pub struct Network<T: DuplexTransport> {
    pub network_type: NetworkType,
    pub web3: Web3<T>,
    pub account: Address,
    pub token: Contract<T>,
    pub relay: Contract<T>,
    pub free: bool,
    pub confirmations: u64,
    pub anchor_frequency: u64,
    pub interval: u64,
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

        let token = Contract::from_json(web3.eth(), token_address, token_abi.as_bytes())
            .or(Err(OperationError::InvalidContractAbi))?;

        let relay = Contract::from_json(web3.eth(), relay_address, relay_abi.as_bytes())
            .or(Err(OperationError::InvalidContractAbi))?;

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

    pub fn handle_transfers(&self, target: &Rc<Network<T>>, handle: &reactor::Handle) -> HandleTransfers {
        HandleTransfers::new(self, target, handle)
    }

    pub fn handle_missed_transfers(&self, target: &Rc<Network<T>>, handle: &reactor::Handle) -> HandleMissedTransfers {
        HandleMissedTransfers::new(self, target, handle)
    }

    pub fn handle_anchors(&self, target: &Rc<Network<T>>, handle: &reactor::Handle) -> HandleAnchors {
        HandleAnchors::new(self, target, handle)
    }

    pub fn get_gas_limit(&self) -> U256 {
        GAS_LIMIT.into()
    }

    pub fn get_gas_price(&self) -> U256 {
        if self.free {
            return FREE_GAS_PRICE.into();
        }
        DEFAULT_GAS_PRICE.into()
    }
}
