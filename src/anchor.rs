use std::fmt;
use std::rc::Rc;
use tokio_core::reactor;
use web3::contract::Options;
use web3::futures::prelude::*;
use web3::futures::sync::mpsc;
use web3::types::{BlockId, BlockNumber, H256, U256};
use web3::DuplexTransport;

use super::relay::Network;

/// Represents a block on the sidechain to be anchored to the homechain
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct Anchor {
    block_hash: H256,
    block_number: U256,
}

impl Anchor {
    fn post<T: DuplexTransport + 'static>(&self, target: &Rc<Network<T>>) -> PostAnchor {
        PostAnchor::new(self, target)
    }
}

impl fmt::Display for Anchor {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "(#{}, hash: {:?})", self.block_number, self.block_hash)
    }
}

pub struct HandleAnchors(Box<Future<Item = (), Error = ()>>);

impl HandleAnchors {
    pub fn new<T: DuplexTransport + 'static>(
        source: &Network<T>,
        target: &Rc<Network<T>>,
        handle: &reactor::Handle,
    ) -> Self {
        let handle = handle.clone();
        let target = target.clone();
        let future = FindAnchors::new(source, &handle).for_each(move |anchor| {
            handle.spawn(anchor.post(&target));
            Ok(())
        });
        HandleAnchors(Box::new(future))
    }
}

impl Future for HandleAnchors {
    type Item = ();
    type Error = ();
    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        self.0.poll()
    }
}

pub struct FindAnchors(mpsc::UnboundedReceiver<Anchor>);

impl FindAnchors {
    pub fn new<T: DuplexTransport + 'static>(source: &Network<T>, handle: &reactor::Handle) -> Self {
        let (tx, rx) = mpsc::unbounded();
        let future = {
            let network_type = source.network_type;
            let anchor_frequency = source.anchor_frequency;
            let confirmations = source.confirmations;
            let handle = handle.clone();
            let web3 = source.web3.clone();

            source
                .web3
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
        FindAnchors(rx)
    }
}

impl Stream for FindAnchors {
    type Item = Anchor;
    type Error = ();
    fn poll(&mut self) -> Poll<Option<Self::Item>, Self::Error> {
        self.0.poll()
    }
}

/// Anchor a sidechain block and return a future which resolves when the transaction completes
pub struct PostAnchor(Box<Future<Item = (), Error = ()>>);

impl PostAnchor {
    pub fn new<T: DuplexTransport + 'static>(anchor: &Anchor, target: &Rc<Network<T>>) -> Self {
        let anchor = *anchor;
        let target = target.clone();
        info!("anchoring block {}", anchor);
        let future = target
            .relay
            .call_with_confirmations(
                "anchor",
                (anchor.block_hash, anchor.block_number),
                target.account,
                Options::with(|options| {
                    options.gas = Some(target.get_gas_limit());
                    options.gas_price = Some(target.get_gas_price());
                }),
                target.confirmations as usize,
            ).and_then(|receipt| {
                info!("anchor processed: {:?}", receipt);
                Ok(())
            }).or_else(|e| {
                error!("error anchoring block: {}", e);
                Ok(())
            });
        PostAnchor(Box::new(future))
    }
}

impl Future for PostAnchor {
    type Item = ();
    type Error = ();
    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        self.0.poll()
    }
}
