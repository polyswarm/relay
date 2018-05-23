use std::rc::Rc;
use std::time;
use tokio_core::reactor;
use web3::confirm::wait_for_transaction_confirmation;
use web3::contract::Contract;
use web3::futures::sync::mpsc;
use web3::futures::{Future, Stream};
use web3::types::{Address, FilterBuilder, H256, U256};
use web3::{DuplexTransport, Web3};

use super::contracts::{ERC20_ABI, ERC20_RELAY_ABI, TRANSFER_EVENT_SIGNATURE};
use super::errors::*;

type Task = Box<Future<Item = (), Error = ()>>;

// From ethereum_types but not reexported by web3
fn clean_0x(s: &str) -> &str {
    if s.starts_with("0x") {
        &s[2..]
    } else {
        s
    }
}

#[derive(Debug, Clone)]
pub struct Relay<T: DuplexTransport> {
    homechain: Rc<Network<T>>,
    sidechain: Rc<Network<T>>,
}

impl<T: DuplexTransport + 'static> Relay<T> {
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
    ) -> Task {
        Box::new({
            chain_a
                .transfer_stream(handle)
                .for_each(move |transfer| {
                    chain_b.process_withdrawal(&transfer);
                    Ok(())
                })
                .map_err(move |e| {
                    error!(
                        "error processing withdrawal from {:?}: {:?}",
                        chain_a.network_type(),
                        e
                    )
                })
        })
    }

    fn anchor_future(
        homechain: Rc<Network<T>>,
        sidechain: Rc<Network<T>>,
        handle: &reactor::Handle,
    ) -> Task {
        Box::new({
            sidechain
                .anchor_stream(handle)
                .for_each(move |anchor| {
                    homechain.anchor(&anchor);
                    Ok(())
                })
                .map_err(move |e| {
                    error!(
                        "error anchoring from {:?}: {:?}",
                        sidechain.network_type(),
                        e
                    )
                })
        })
    }

    pub fn listen(&self, handle: &reactor::Handle) -> Task {
        Box::new(
            Self::anchor_future(self.homechain.clone(), self.sidechain.clone(), handle)
                .join(
                    Self::transfer_future(self.homechain.clone(), self.sidechain.clone(), handle)
                        .join(Self::transfer_future(
                            self.sidechain.clone(),
                            self.homechain.clone(),
                            handle,
                        )),
                )
                .and_then(|_| Ok(())),
        )
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct Transfer {
    destination: Address,
    amount: U256,
    tx_hash: H256,
    block_hash: H256,
    block_number: U256,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct Anchor {
    block_hash: H256,
    block_number: U256,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum NetworkType {
    Home,
    Side,
}

#[derive(Debug)]
pub struct Network<T: DuplexTransport> {
    network_type: NetworkType,
    web3: Web3<T>,
    token: Contract<T>,
    relay: Contract<T>,
    confirmations: u32,
    anchor_frequency: u32,
}

impl<T: DuplexTransport + 'static> Network<T> {
    pub fn new(
        network_type: NetworkType,
        transport: T,
        token: &str,
        relay: &str,
        confirmations: u32,
        anchor_frequency: u32,
    ) -> Result<Self> {
        let web3 = Web3::new(transport);
        let token_address: Address = clean_0x(token)
            .parse()
            .chain_err(|| ErrorKind::InvalidAddress(token.to_owned()))?;
        let relay_address: Address = clean_0x(relay)
            .parse()
            .chain_err(|| ErrorKind::InvalidAddress(relay.to_owned()))?;

        let token = Contract::from_json(web3.eth(), token_address, ERC20_ABI.as_bytes())
            .chain_err(|| ErrorKind::InvalidContractAbi)?;
        let relay = Contract::from_json(web3.eth(), relay_address, ERC20_RELAY_ABI.as_bytes())
            .chain_err(|| ErrorKind::InvalidContractAbi)?;

        Ok(Self {
            network_type,
            web3,
            token,
            relay,
            confirmations,
            anchor_frequency,
        })
    }

    pub fn homechain(
        transport: T,
        token: &str,
        relay: &str,
        confirmations: u32,
        anchor_frequency: u32,
    ) -> Result<Self> {
        Self::new(
            NetworkType::Home,
            transport,
            token,
            relay,
            confirmations,
            anchor_frequency,
        )
    }

    pub fn sidechain(
        transport: T,
        token: &str,
        relay: &str,
        confirmations: u32,
        anchor_frequency: u32,
    ) -> Result<Self> {
        Self::new(
            NetworkType::Side,
            transport,
            token,
            relay,
            confirmations,
            anchor_frequency,
        )
    }

    pub fn network_type(&self) -> NetworkType {
        self.network_type
    }

    pub fn transfer_stream(
        &self,
        handle: &reactor::Handle,
    ) -> Box<Stream<Item = Transfer, Error = ()>> {
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
                                let destination: Address = log.topics[2].into();
                                let amount: U256 = log.data.0[..32].into();

                                trace!("received transfer event in tx hash {:?}, waiting for confirmations", &tx_hash);

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

                                        trace!("{:?}", &transfer);
                                        tx.unbounded_send(transfer).unwrap();
                                        Ok(())
                                    })
                                        .map_err(|_| ()),
                                );

                                Ok(())
                            },
                        )
                    })
                })
                .map_err(move |e| error!("error in {:?} transfer stream: {:?}", network_type, e))
        };

        handle.spawn(future);

        Box::new(rx)
    }

    pub fn anchor_stream(
        &self,
        handle: &reactor::Handle,
    ) -> Box<Stream<Item = Anchor, Error = ()>> {
        let (tx, rx) = mpsc::unbounded();

        let future = {
            let network_type = self.network_type;
            let tx = tx.clone();

            self.web3
                .eth_subscribe()
                .subscribe_new_heads()
                .and_then(move |sub| {
                    sub.for_each(move |head| {
                        if head.number.is_none() {
                            warn!("no block number in anchor");
                            return Ok(());
                        }

                        if head.hash.is_none() {
                            warn!("no block hash in anchor");
                            return Ok(());
                        }

                        let block_number: U256 = head.number.unwrap().into();
                        let block_hash: H256 = head.hash.unwrap();

                        let anchor = Anchor {
                            block_hash,
                            block_number,
                        };

                        trace!("{:?}", &anchor);
                        tx.unbounded_send(anchor).unwrap();

                        Ok(())
                    })
                })
                .map_err(move |e| error!("error in {:?} anchor stream: {:?}", network_type, e))
        };

        handle.spawn(future);

        Box::new(rx)
    }

    pub fn process_withdrawal(&self, transfer: &Transfer) -> Box<Future<Item = (), Error = Error>> {
        trace!("processing withdrawal {:?}", transfer);
        Box::new(::web3::futures::future::err("not implemented".into()))
    }

    pub fn anchor(&self, anchor: &Anchor) -> Box<Future<Item = (), Error = Error>> {
        trace!("anchoring block {:?}", anchor);
        Box::new(::web3::futures::future::err("not implemented".into()))
    }
}
