use ethabi::Token;
use std::fmt;
use std::rc::Rc;
use std::time;
use tiny_keccak::keccak256;
use tokio_core::reactor;
use web3::confirm::wait_for_transaction_confirmation;
use web3::contract;
use web3::contract::tokens::Detokenize;
use web3::contract::{Contract, Options};
use web3::futures::sync::mpsc;
use web3::futures::{Future, Stream};
use web3::types::{Address, BlockId, BlockNumber, FilterBuilder, H256, U256};
use web3::{DuplexTransport, Web3};

use failure::{Error, SyncFailure};

use super::contracts::TRANSFER_EVENT_SIGNATURE;
use super::errors::OperationError;

const GAS_LIMIT: u64 = 200_000;
const DEFAULT_GAS_PRICE: u64 = 20_000_000_000;
const FREE_GAS_PRICE: u64 = 0;
const LOOKBACK_LEEWAY: u64 = 10;

// From ethereum_types but not reexported by web3
fn clean_0x(s: &str) -> &str {
    if s.starts_with("0x") {
        &s[2..]
    } else {
        s
    }
}

/// Withdrawal event added to contract after a transfer
#[derive(Debug, Clone)]
pub struct Withdrawal {
    destination: Address,
    amount: U256,
    processed: bool,
}

impl Detokenize for Withdrawal {
    /// Creates a new instance from parsed ABI tokens.
    fn from_tokens(tokens: Vec<Token>) -> Result<Self, contract::Error>
    where
        Self: Sized,
    {
        let destination =
            tokens[0]
                .clone()
                .to_address()
                .ok_or_else(|| contract::Error::from_kind(contract::ErrorKind::Msg(
                    "cannot parse destination address from contract response".to_string(),
                )))?;
        let amount = tokens[1]
            .clone()
            .to_uint()
            .ok_or_else(|| contract::Error::from_kind(contract::ErrorKind::Msg(
                "cannot parse amount uint from contract response".to_string(),
            )))?;
        let processed = tokens[2]
            .clone()
            .to_bool()
            .ok_or_else(|| contract::Error::from_kind(contract::ErrorKind::Msg(
                "cannot parse processed bool from contract response".to_string(),
            )))?;
        Ok(Withdrawal {
            destination,
            amount,
            processed,
        })
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

    fn transfer_future(
        chain_a: &Rc<Network<T>>,
        chain_b: Rc<Network<T>>,
        handle: &reactor::Handle,
    ) -> impl Future<Item = (), Error = ()> {
        let h = handle.clone();
        chain_a.transfer_stream(handle).for_each(move |transfer| {
            h.spawn(chain_b.approve_withdrawal(&transfer));
            Ok(())
        })
    }

    fn missed_transfer_future(
        chain_a: &Rc<Network<T>>,
        chain_b: Rc<Network<T>>,
        handle: &reactor::Handle,
    ) -> impl Future<Item = (), Error = ()> {
        let handle = handle.clone();
        let chain_a = chain_a.clone();
        chain_a.missed_transfer_stream(&handle)
        .for_each(move |transfer| {
            let chain_b = chain_b.clone();
            let handle = handle.clone();
            chain_b
                .get_withdrawal_future(&transfer)
                .and_then(move |withdrawal| {
                    info!("Found withdrawal: {:?}", &withdrawal);
                    if withdrawal.processed {
                        info!("skipping already processed transaction {:?}", &transfer.tx_hash);
                        Ok(None)
                    } else {
                        Ok(Some(transfer))
                    }
                }).and_then(move |transfer_option| {
                    let chain_b = chain_b.clone();
                    let handle = handle.clone();
                    if let Some(transfer) = transfer_option {
                        handle.spawn(chain_b.approve_withdrawal(&transfer))
                    }
                    Ok(())
                })
        })
    }

    fn anchor_future(
        sidechain: &Rc<Network<T>>,
        homechain: Rc<Network<T>>,
        handle: &reactor::Handle,
    ) -> impl Future<Item = (), Error = ()> {
        let h = handle.clone();
        sidechain.anchor_stream(handle).for_each(move |anchor| {
            h.spawn(homechain.anchor(&anchor));
            Ok(())
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
        Self::anchor_future(&self.sidechain, self.homechain.clone(), handle)
            .join(
                Self::transfer_future(&self.homechain, self.sidechain.clone(), handle)
                    .join(Self::transfer_future(&self.sidechain, self.homechain.clone(), handle))
                    .join(Self::missed_transfer_future(
                        &self.homechain,
                        self.sidechain.clone(),
                        handle,
                    )).join(Self::missed_transfer_future(
                        &self.sidechain,
                        self.homechain.clone(),
                        handle,
                    )),
            ).and_then(|_| Ok(()))
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
pub struct Network<T: DuplexTransport> {
    network_type: NetworkType,
    web3: Web3<T>,
    account: Address,
    token: Contract<T>,
    relay: Contract<T>,
    free: bool,
    confirmations: u64,
    anchor_frequency: u64,
    interval: u64,
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

    /// Returns a Future to retrieve the withdrawal event for this transaction
    ///
    /// # Arguments
    ///
    /// * `transfer` - A Transfer struct with the tx_hash, block_hash, and block_number we need to retrieve the withdrawal
    pub fn get_withdrawal_future(&self, transfer: &Transfer) -> impl Future<Item = Withdrawal, Error = ()> {
        let tx_hash = transfer.tx_hash;
        let block_hash = transfer.block_hash;
        let block_number: &mut [u8] = &mut [0; 32];
        transfer.block_number.to_big_endian(block_number);
        let mut grouped: Vec<u8> = Vec::new();
        grouped.extend_from_slice(&tx_hash[..]);
        grouped.extend_from_slice(&block_hash[..]);
        grouped.extend_from_slice(block_number);
        let hash = H256(keccak256(&grouped[..]));
        let account = self.account;
        self.relay
            .query::<Withdrawal, Address, BlockNumber, H256>(
                "withdrawals",
                hash,
                account,
                Options::default(),
                BlockNumber::Latest,
            ).or_else(|e| {
                error!("error getting withdrawal: {}", e);
                Err(())
            })
    }

    /// Returns a Stream of Transfer that were missed, either from downtime, or failed approvals
    ///
    /// # Arguments
    ///
    /// * `handle` - Handle to the event loop to spawn additional futures
    pub fn missed_transfer_stream(&self,handle: &reactor::Handle,) -> impl Stream<Item = Transfer, Error = ()> {
        let (tx, rx) = mpsc::unbounded();
        let h = handle.clone();
        let token_address = self.token.address();
        let relay_address = self.relay.address();
        let network_type = self.network_type;
        let interval = self.interval;
        let confirmations = self.confirmations;
        let web3 = self.web3.clone();
        let future =
        web3.clone().eth_subscribe()
        .subscribe_new_heads()
        .and_then(move |sub| {
            let handle = h.clone();
            let tx = tx.clone();
            let web3 = web3.clone();
            sub.chunks(interval as usize).for_each(move |_| {
                let handle = handle.clone();
                let tx = tx.clone();
                let web3 = web3.clone();
                web3.clone()
                    .eth()
                    .block_number()
                    .and_then(move |block| {
                        info!("Scanning at block {}", block);
                        let from = block.as_u64() - confirmations - interval - LOOKBACK_LEEWAY;
                        let to = block.as_u64() - confirmations;
                        let filter = FilterBuilder::default()
                            .address(vec![token_address])
                            .from_block(BlockNumber::from(from))
                            .to_block(BlockNumber::from(to))
                            .topics(
                                Some(vec![TRANSFER_EVENT_SIGNATURE.into()]),
                                None,
                                Some(vec![relay_address.into()]),
                                None,
                            ).build();
                        let web3 = web3.clone();
                        web3.clone().eth().logs(filter).and_then(move |logs| {
                            let tx = tx.clone();
                            let web3 = web3.clone();
                            let handle = handle.clone();
                            info!("found {} log(s)", &logs.len());
                            logs.iter().for_each(|log| {
                                if Some(true) == log.removed {
                                    warn!("found removed log");
                                    return;
                                }

                                log.transaction_hash.map_or_else(
                                    || {
                                        warn!("log missing transaction hash");
                                        return;
                                    },
                                    |tx_hash| {
                                        let tx = tx.clone();
                                        let destination: Address = log.topics[1].into();
                                        let amount: U256 = log.data.0[..32].into();
                                        info!("found transfer event in tx hash {:?}, checking for approval", &tx_hash);

                                        handle.spawn(
                                            web3.eth()
                                                .transaction_receipt(tx_hash)
                                                .and_then(move |transaction_receipt| {
                                                    let tx_hash = tx_hash;
                                                    transaction_receipt.map_or_else(
                                                        || {
                                                            error!(
                                                                "no receipt found for transaction hash {}",
                                                                &tx_hash
                                                            );
                                                            Ok(())
                                                        },
                                                        |receipt| {
                                                            if receipt.block_number.is_none() {
                                                                warn!("no block number in transfer receipt");
                                                                return Ok(());
                                                            }

                                                            if receipt.block_hash.is_none() {
                                                                warn!("no block hash in transfer receipt");
                                                                return Ok(());
                                                            }

                                                            let block_hash = receipt.block_hash.unwrap();
                                                            let block_number = receipt.block_number.unwrap();

                                                            let transfer = Transfer {
                                                                destination,
                                                                amount,
                                                                tx_hash,
                                                                block_hash,
                                                                block_number,
                                                            };
                                                            info!(
                                                                "transfer event not yet approved, approving {}",
                                                                &transfer
                                                            );
                                                            tx.unbounded_send(transfer).unwrap();
                                                            Ok(())
                                                        },
                                                    )
                                                }).or_else(|e| {
                                                    error!("error approving transaction: {}", e);
                                                    Ok(())
                                                })
                                        );
                                    },
                                );
                            });
                            Ok(())
                        })
                    }).or_else(move |e| {
                        error!("error in {:?} transfer logs: {}", network_type, e);
                        Ok(())
                    })
            }).or_else(move |e| {
                error!("error in {:?} transfer logs: {}", network_type, e);
                Ok(())
            })
        }).or_else(move |e| {
            error!("error checking every {} blocks: {}", interval, e);
            Ok(())
        });
        handle.spawn(future);
        rx
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
            ).build();

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

                                handle.spawn(
                                    wait_for_transaction_confirmation(
                                        transport.clone(),
                                        tx_hash,
                                        time::Duration::from_secs(1),
                                        confirmations,
                                    ).and_then(move |receipt| {
                                        if receipt.block_number.is_none() {
                                            warn!("no block number in transfer receipt");
                                            return Ok(());
                                        }

                                        if receipt.block_hash.is_none() {
                                            warn!("no block hash in transfer receipt");
                                            return Ok(());
                                        }

                                        let block_hash = receipt.block_hash.unwrap();
                                        let block_number = receipt.block_number.unwrap();

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
                                    }).or_else(|e| {
                                        error!("error waiting for transfer confirmations: {}", e);
                                        Ok(())
                                    }),
                                );

                                Ok(())
                            },
                        )
                    })
                }).or_else(move |e| {
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
                                                .and_then(move |block| match block {
                                                    Some(b) => {
                                                        if b.number.is_none() {
                                                            warn!("no block number in anchor block");
                                                            return Ok(());
                                                        }

                                                        if b.hash.is_none() {
                                                            warn!("no block hash in anchor block");
                                                            return Ok(());
                                                        }

                                                        let block_hash: H256 = b.hash.unwrap();
                                                        let block_number: U256 = b.number.unwrap().into();

                                                        let anchor = Anchor {
                                                            block_hash,
                                                            block_number,
                                                        };

                                                        info!("anchor block confirmed, anchoring: {}", &anchor);

                                                        tx.unbounded_send(anchor).unwrap();
                                                        Ok(())
                                                    }
                                                    None => {
                                                        warn!("no block found for anchor confirmations");
                                                        Ok(())
                                                    }
                                                }).or_else(|e| {
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
                }).or_else(move |e| {
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
    pub fn approve_withdrawal(&self, transfer: &Transfer) -> impl Future<Item = (), Error = ()> {
        info!("approving withdrawal {}", transfer);
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
                    options.gas_price = Some(self.get_gas_price());
                }),
                self.confirmations as usize,
            ).and_then(|receipt| {
                info!("withdrawal approved: {:?}", receipt);
                Ok(())
            }).or_else(|e| {
                error!("error approving withdrawal: {}", e);
                Ok(())
            })
    }

    /// Anchor a sidechain block and return a future which resolves when the transaction completes
    ///
    /// # Arguments
    ///
    /// * `anchor` - The block to anchor
    pub fn anchor(&self, anchor: &Anchor) -> impl Future<Item = (), Error = ()> {
        info!("anchoring block {}", anchor);
        self.relay
            .call_with_confirmations(
                "anchor",
                (anchor.block_hash, anchor.block_number),
                self.account,
                Options::with(|options| {
                    options.gas = Some(GAS_LIMIT.into());
                    options.gas_price = Some(self.get_gas_price());
                }),
                self.confirmations as usize,
            ).and_then(|receipt| {
                info!("anchor processed: {:?}", receipt);
                Ok(())
            }).or_else(|e| {
                error!("error anchoring block: {}", e);
                Ok(())
            })
    }

    fn get_gas_price(&self) -> U256 {
        if self.free {
            return FREE_GAS_PRICE.into();
        }
        DEFAULT_GAS_PRICE.into()
    }
}
