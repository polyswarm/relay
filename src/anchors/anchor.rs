use ethabi::Token;
use std::fmt;
use std::rc::Rc;
use tokio_core::reactor;
use web3::contract::tokens::Tokenize;
use web3::futures::prelude::*;
use web3::futures::sync::mpsc;
use web3::futures::try_ready;
use web3::types::{BlockId, BlockNumber, H256, U256};
use web3::DuplexTransport;

use super::eth::transaction::SendTransaction;
use super::extensions::timeout::Timeout;
use super::relay::Network;

/// Represents a block on the sidechain to be anchored to the homechain
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct Anchor {
    block_hash: H256,
    block_number: U256,
}

impl Anchor {
    ///Returns a ProcessAnchor Future to post the anchor to the ERC20Relay contract
    ///
    /// # Arguments
    ///
    /// * `target` - Network to post the anchor
    fn process<T: DuplexTransport + 'static>(&self, target: &Rc<Network<T>>) -> SendTransaction<T, Self> {
        info!("anchoring block {} to {:?}", self, target.network_type);
        SendTransaction::new(target, "anchor", self, target.retries)
    }
}

impl Tokenize for Anchor {
    fn into_tokens(self) -> Vec<Token> {
        vec![
            Token::FixedBytes(self.block_hash.to_vec()),
            Token::Uint(self.block_number),
        ]
    }
}

impl fmt::Display for Anchor {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "(#{}, hash: {:?})", self.block_number, self.block_hash)
    }
}

/// Future to handle the Stream of anchors & post them to the chain
pub struct HandleAnchors<T: DuplexTransport + 'static> {
    target: Rc<Network<T>>,
    stream: FindAnchors,
    handle: reactor::Handle,
}

impl<T: DuplexTransport + 'static> HandleAnchors<T> {
    /// Returns a newly created HandleAnchors Future
    ///
    /// # Arguments
    ///
    /// * `source` - Network where the block headers are captured
    /// * `target` - Network where the headers will be anchored
    /// * `handle` - Handle to spawn new futures
    pub fn new(source: &Network<T>, target: &Rc<Network<T>>, handle: &reactor::Handle) -> Self {
        let handle = handle.clone();
        let target = target.clone();
        let stream = FindAnchors::new(source, &handle);
        HandleAnchors { target, stream, handle }
    }
}

impl<T: DuplexTransport + 'static> Future for HandleAnchors<T> {
    type Item = ();
    type Error = ();
    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        loop {
            let anchor = try_ready!(self.stream.poll());
            match anchor {
                Some(a) => {
                    self.handle.spawn(a.process(&self.target));
                }
                None => {
                    return Err(());
                }
            }
        }
    }
}

/// Stream of block headers on one chain to be posted to the other
pub struct FindAnchors(mpsc::UnboundedReceiver<Anchor>);

impl FindAnchors {
    /// Returns a newly created FindAnchors Future
    ///
    /// # Arguments
    ///
    /// * `source` - Network where the block headers are found
    /// * `handle` - Handle to spawn new futures
    pub fn new<T: DuplexTransport + 'static>(source: &Network<T>, handle: &reactor::Handle) -> Self {
        let (tx, rx) = mpsc::unbounded();
        let future = {
            let network_type = source.network_type;
            let anchor_frequency = source.anchor_frequency;
            let confirmations = source.confirmations;
            let h = handle.clone();
            let handle = handle.clone();
            let web3 = source.web3.clone();
            let timeout = source.timeout;

            source
                .web3
                .eth_subscribe()
                .subscribe_new_heads()
                .timeout(timeout, &h)
                .for_each(move |head| {
                    head.number.map_or_else(
                        || {
                            warn!("no block number in block head event on {:?}", network_type);
                            Ok(())
                        },
                        |block_number| {
                            let tx = tx.clone();

                            match block_number.checked_rem(anchor_frequency.into()).map(|u| u.low_u64()) {
                                Some(c) if c == confirmations => {
                                    let block_id =
                                        BlockId::Number(BlockNumber::Number(block_number.low_u64() - confirmations));

                                    handle.spawn(
                                        web3.eth()
                                            .block(block_id)
                                            .and_then(move |block| match block {
                                                Some(b) => {
                                                    if b.number.is_none() {
                                                        warn!("no block number in anchor block on {:?}", network_type);
                                                        return Ok(());
                                                    }

                                                    if b.hash.is_none() {
                                                        warn!("no block hash in anchor block on {:?}", network_type);
                                                        return Ok(());
                                                    }

                                                    let block_hash: H256 = b.hash.unwrap();
                                                    let block_number: U256 = b.number.unwrap().into();

                                                    let anchor = Anchor {
                                                        block_hash,
                                                        block_number,
                                                    };

                                                    info!(
                                                        "anchor block confirmed, anchoring on {:?}: {}",
                                                        network_type, &anchor
                                                    );

                                                    tx.unbounded_send(anchor).unwrap();
                                                    Ok(())
                                                }
                                                None => {
                                                    warn!(
                                                        "no block found for anchor confirmations on {:?}",
                                                        network_type
                                                    );
                                                    Ok(())
                                                }
                                            })
                                            .or_else(move |e| {
                                                error!(
                                                    "error waiting for anchor confirmations on {:?}: {:?}",
                                                    network_type, e
                                                );
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
                .map_err(move |e| {
                    error!("error in anchor stream on {:?}: {:?}", network_type, e);
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
