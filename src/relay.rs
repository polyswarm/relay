use std::rc::Rc;
use tokio_core::reactor;
use web3::{DuplexTransport, Web3};
use web3::contract::Contract;
use web3::futures::{Future, Stream};
use web3::futures::sync::mpsc;
use web3::types::{Address, FilterBuilder, H256, U256};

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
    tx_hash: H256,
    destination: Address,
    amount: U256,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct Anchor {
    block_number: U256,
    block_hash: H256,
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
}

impl<T: DuplexTransport + 'static> Network<T> {
    pub fn new(network_type: NetworkType, transport: T, token: &str, relay: &str) -> Result<Self> {
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
        })
    }

    pub fn homechain(transport: T, token: &str, relay: &str) -> Result<Self> {
        Self::new(NetworkType::Home, transport, token, relay)
    }

    pub fn sidechain(transport: T, token: &str, relay: &str) -> Result<Self> {
        Self::new(NetworkType::Side, transport, token, relay)
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
            let tx = tx.clone();
            //let handle = handle.clone();

            self.web3
                .eth_subscribe()
                .subscribe_logs(filter)
                .and_then(move |sub| {
                    sub.for_each(move |log| {
                        if Some(true) == log.removed {
                            warn!("received removed log, revoke votes");
                            return Ok(());
                        }

                        if log.transaction_hash.is_none() {
                            warn!("no transaction hash in transfer");
                            return Ok(());
                        }

                        let tx_hash = log.transaction_hash.unwrap();
                        let destination: Address = log.topics[2].into();
                        let amount: U256 = log.data.0[..32].into();

                        let transfer = Transfer {
                            tx_hash,
                            destination,
                            amount,
                        };

                        trace!("{:?}", &transfer);
                        tx.unbounded_send(transfer).unwrap();

                        Ok(())
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
                            block_number,
                            block_hash,
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
        println!("{:?}", transfer);
        Box::new(::web3::futures::future::err("not implemented".into()))
    }

    pub fn anchor(&self, anchor: &Anchor) -> Box<Future<Item = (), Error = Error>> {
        println!("{:?}", anchor);
        Box::new(::web3::futures::future::err("not implemented".into()))
    }
}

impl<T: DuplexTransport> Drop for Network<T> {
    fn drop(&mut self) {
        println!("DROPPING");
    }
}
