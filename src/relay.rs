use std::fmt;
use std::rc::Rc;
use std::time;
use tokio_core::reactor;
use web3::confirm::wait_for_transaction_confirmation;
use web3::contract::{Contract, Options};
use web3::futures::sync::mpsc;
use web3::futures::{Future, Stream};
use web3::types::{Address, BlockId, BlockNumber, FilterBuilder, H256, U256};
use web3::{DuplexTransport, Web3};

use super::contracts::{TRANSFER_EVENT_SIGNATURE};
use super::errors::*;
use super::consul_config::{get_contract_abi};

const GAS_LIMIT: u64 = 200000;
const GAS_PRICE: u64 = 20000000000;

// From ethereum_types but not reexported by web3
fn clean_0x(s: &str) -> &str {
    if s.starts_with("0x") {
        &s[2..]
    } else {
        s
    }
}

/// Token relay between two Ethereum networks
#[derive(Debug, Clone)]
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

    fn transfer_future(
        chain_a: Rc<Network<T>>,
        chain_b: Rc<Network<T>>,
        handle: &reactor::Handle,
    ) -> impl Future<Item = (), Error = ()> {
        chain_a.transfer_stream(handle).for_each(move |transfer| {
            chain_b.approve_withdrawal(&transfer).or_else(|e| {
                error!("error approving withdrawal: {}", e);
                Ok(())
            })
        })
    }

    fn anchor_future(
        homechain: Rc<Network<T>>,
        sidechain: Rc<Network<T>>,
        handle: &reactor::Handle,
    ) -> impl Future<Item = (), Error = ()> {
        sidechain.anchor_stream(handle).for_each(move |anchor| {
            homechain.anchor(&anchor).or_else(|e| {
                error!("error anchoring block: {}", e);
                Ok(())
            })
        })
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
        Self::anchor_future(self.homechain.clone(), self.sidechain.clone(), handle)
            .join(
                Self::transfer_future(self.homechain.clone(), self.sidechain.clone(), handle).join(
                    Self::transfer_future(self.sidechain.clone(), self.homechain.clone(), handle),
                ),
            )
            .and_then(|_| Ok(()))
    }
}

/// Represents a token transfer between two networks
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct Transfer {
    destination: Address,
    amount: U256,
    tx_hash: H256,
    block_hash: H256,
    block_number: U256,
}

impl fmt::Display for Transfer {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "({} -> {:?}, hash: {:?})",
            self.amount, self.destination, self.tx_hash
        )
    }
}

/// Represents a block on the sidechain to be anchored to the homechain
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct Anchor {
    block_hash: H256,
    block_number: U256,
}

impl fmt::Display for Anchor {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "(#{}, hash: {:?})", self.block_number, self.block_hash)
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
#[derive(Debug)]
pub struct Network<T: DuplexTransport> {
    network_type: NetworkType,
    web3: Web3<T>,
    account: Address,
    token: Contract<T>,
    relay: Contract<T>,
    confirmations: u64,
    anchor_frequency: u64,
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
    pub fn new(
        network_type: NetworkType,
        transport: T,
        account: &str,
        token: &str,
        relay: &str,
        confirmations: u64,
        anchor_frequency: u64,
    ) -> Result<Self> {
        let web3 = Web3::new(transport);
        let account = clean_0x(account)
            .parse()
            .chain_err(|| ErrorKind::InvalidAddress(account.to_owned()))?;
        let token_address: Address = clean_0x(token)
            .parse()
            .chain_err(|| ErrorKind::InvalidAddress(token.to_owned()))?;
        let relay_address: Address = clean_0x(relay)
            .parse()
            .chain_err(|| ErrorKind::InvalidAddress(relay.to_owned()))?;
        let token = Contract::from_json(web3.eth(), token_address, get_contract_abi("ERC20Relay".to_string())
.as_bytes())
            .chain_err(|| ErrorKind::InvalidContractAbi)?;
        let relay = Contract::from_json(web3.eth(), relay_address, get_contract_abi("NectarToken".to_string())
.as_bytes())
            .chain_err(|| ErrorKind::InvalidContractAbi)?;

        Ok(Self {
            network_type,
            web3,
            account,
            token,
            relay,
            confirmations,
            anchor_frequency,
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
    pub fn homechain(transport: T, account: &str, token: &str, relay: &str, confirmations: u64) -> Result<Self> {
        Self::new(NetworkType::Home, transport, account, token, relay, confirmations, 0)
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
        relay: &str,
        confirmations: u64,
        anchor_frequency: u64,
    ) -> Result<Self> {
        Self::new(
            NetworkType::Side,
            transport,
            account,
            token,
            relay,
            confirmations,
            anchor_frequency,
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
            .map_err(|e| e.into())
            .and_then(move |success| {
                if !success {
                    return Err(ErrorKind::CouldNotUnlockAccount(account).into());
                }
                Ok(())
            })
    }

    /// Returns a Stream of Transfer events from this network to the relay contract
    ///
    /// # Arguments
    ///
    /// * `handle` - Handle to the event loop to spawn additional futures
    pub fn transfer_stream(&self, handle: &reactor::Handle) -> impl Stream<Item = Transfer, Error = ()> {
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

        let future = {
            let network_type = self.network_type;
            let confirmations = self.confirmations as usize;
            let handle = handle.clone();
            let transport = self.web3.transport().clone();

            self.web3
                .eth_subscribe()
                .subscribe_logs(filter)
                .and_then(move |sub| {
                    sub.for_each(move |log| {
                        if Some(true) == log.removed {
                            warn!("received removed log, revoke votes");
                            return Ok(());
                        }

                        log.transaction_hash.map_or_else(
                            || {
                                warn!("log missing transaction hash");
                                Ok(())
                            },
                            |tx_hash| {
                                let tx = tx.clone();
                                let destination: Address = log.topics[1].into();
                                let amount: U256 = log.data.0[..32].into();

                                info!(
                                    "received transfer event in tx hash {:?}, waiting for confirmations",
                                    &tx_hash
                                );

                                &handle.spawn(
                                    wait_for_transaction_confirmation(
                                        transport.clone(),
                                        tx_hash,
                                        time::Duration::from_secs(1),
                                        confirmations,
                                    ).and_then(move |receipt| {
                                        let block_hash = receipt.block_hash;
                                        let block_number = receipt.block_number;

                                        let transfer = Transfer {
                                            destination,
                                            amount,
                                            tx_hash,
                                            block_hash,
                                            block_number,
                                        };

                                        info!("transfer event confirmed, approving {}", &transfer);
                                        tx.unbounded_send(transfer).unwrap();
                                        Ok(())
                                    })
                                        .or_else(|e| {
                                            error!("error waiting for transfer confirmations: {}", e);
                                            Ok(())
                                        }),
                                );

                                Ok(())
                            },
                        )
                    })
                })
                .or_else(move |e| {
                    error!("error in {:?} transfer stream: {}", network_type, e);
                    Ok(())
                })
        };

        handle.spawn(future);
        rx
    }

    /// Returns a Stream of anchor blocks from this network
    ///
    /// # Arguments
    ///
    /// * `handle` - Handle to the event loop to spawn additional futures
    pub fn anchor_stream(&self, handle: &reactor::Handle) -> impl Stream<Item = Anchor, Error = ()> {
        let (tx, rx) = mpsc::unbounded();

        let future = {
            let network_type = self.network_type;
            let anchor_frequency = self.anchor_frequency;
            let confirmations = self.confirmations;
            let handle = handle.clone();
            let web3 = self.web3.clone();

            self.web3
                .eth_subscribe()
                .subscribe_new_heads()
                .and_then(move |sub| {
                    sub.for_each(move |head| {
                        head.number.map_or_else(
                            || {
                                warn!("no block number in block head event");
                                Ok(())
                            },
                            |block_number| {
                                let tx = tx.clone();

                                match block_number.checked_rem(anchor_frequency.into()).map(|u| u.low_u64()) {
                                    Some(c) if c == confirmations => {
                                        let block_id = BlockId::Number(BlockNumber::Number(
                                            block_number.low_u64() - confirmations,
                                        ));

                                        handle.spawn(
                                            web3.eth()
                                                .block(block_id)
                                                .and_then(move |block| {
                                                    if block.number.is_none() {
                                                        warn!("no block number in anchor block");
                                                        return Ok(());
                                                    }

                                                    if block.hash.is_none() {
                                                        warn!("no block hash in anchor block");
                                                        return Ok(());
                                                    }

                                                    let block_hash: H256 = block.hash.unwrap();
                                                    let block_number: U256 = block.number.unwrap().into();

                                                    let anchor = Anchor {
                                                        block_hash,
                                                        block_number,
                                                    };

                                                    trace!("anchor block confirmed, anchoring: {}", &anchor);

                                                    tx.unbounded_send(anchor).unwrap();
                                                    Ok(())
                                                })
                                                .or_else(|e| {
                                                    error!("error waiting for anchor confirmations: {}", e);
                                                    Ok(())
                                                }),
                                        );
                                    }
                                    _ => (),
                                };

                                Ok(())
                            },
                        )
                    })
                })
                .or_else(move |e| {
                    error!("error in {:?} anchor stream: {}", network_type, e);
                    Ok(())
                })
        };

        handle.spawn(future);
        rx
    }

    /// Approve a withdrawal request and return a future which resolves when the transaction
    /// completes
    ///
    /// # Arguments
    ///
    /// * `transfer` - The transfer to approve
    pub fn approve_withdrawal(&self, transfer: &Transfer) -> impl Future<Item = (), Error = Error> {
        trace!("approving withdrawal {}", transfer);
        self.relay
            .call_with_confirmations(
                "approveWithdrawal",
                (
                    transfer.destination,
                    transfer.amount,
                    transfer.tx_hash,
                    transfer.block_hash,
                    transfer.block_number,
                ),
                self.account,
                Options::with(|options| {
                    options.gas = Some(GAS_LIMIT.into());
                    options.gas_price = Some(GAS_PRICE.into());
                }),
                self.confirmations as usize,
            )
            .and_then(|receipt| {
                info!("withdrawal approved: {:?}", receipt);
                Ok(())
            })
            .or_else(|e| {
                error!("error approving withdrawal: {}", e);
                Ok(())
            })
    }

    /// Anchor a sidechain block and return a future which resolves when the transaction completes
    ///
    /// # Arguments
    ///
    /// * `anchor` - The block to anchor
    pub fn anchor(&self, anchor: &Anchor) -> impl Future<Item = (), Error = Error> {
        trace!("anchoring block {}", anchor);
        self.relay
            .call_with_confirmations(
                "anchor",
                (anchor.block_hash, anchor.block_number),
                self.account,
                Options::with(|options| {
                    options.gas = Some(GAS_LIMIT.into());
                    options.gas_price = Some(GAS_PRICE.into());
                }),
                self.confirmations as usize,
            )
            .and_then(|receipt| {
                info!("anchor processed: {:?}", receipt);
                Ok(())
            })
            .or_else(|e| {
                error!("error anchoring block: {}", e);
                Ok(())
            })
    }
}
