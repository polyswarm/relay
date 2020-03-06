use ethabi::Token;
use std::fmt;
use tokio_core::reactor;
use web3::contract::tokens::Tokenize;
use web3::futures::prelude::*;
use web3::futures::sync::mpsc;
use web3::futures::try_ready;
use web3::types::{BlockHeader, BlockId, BlockNumber, H256, U64};
use web3::DuplexTransport;

use crate::eth::transaction::SendTransaction;
use crate::extensions::flushed::Flushed;
use crate::extensions::timeout::Timeout;
use crate::relay::Network;

/// Represents a block on the sidechain to be anchored to the homechain
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct Anchor {
    block_hash: H256,
    block_number: U64,
}

impl Anchor {
    ///Returns a SendTransaction Future to post the anchor to the ERC20Relay contract
    ///
    /// # Arguments
    ///
    /// * `target` - Network to post the anchor
    fn process<T: DuplexTransport + 'static>(&self, target: &Network<T>) -> Box<dyn Future<Item = (), Error = ()>> {
        info!("anchoring block {} to {:?}", self, target.network_type);
        Box::new(SendTransaction::new(target, "anchor", self, target.retries).or_else(|_| Ok(())))
    }
}

impl Tokenize for Anchor {
    fn into_tokens(self) -> Vec<Token> {
        vec![
            Token::FixedBytes(self.block_hash.0.to_vec()),
            Token::Uint(self.block_number.as_u64().into()),
        ]
    }
}

impl fmt::Display for Anchor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "(#{}, hash: {:?})", self.block_number, self.block_hash)
    }
}

/// Future to handle the Stream of anchors & post them to the chain
pub struct ProcessAnchors<T: DuplexTransport + 'static> {
    source: Network<T>,
    target: Network<T>,
    stream: mpsc::UnboundedReceiver<Anchor>,
    handle: reactor::Handle,
}

impl<T: DuplexTransport + 'static> ProcessAnchors<T> {
    /// Returns a newly created ProcessAnchors Future
    ///
    /// # Arguments
    ///
    /// * `source` - Network where the block headers are captured
    /// * `target` - Network where the headers will be anchored
    /// * `handle` - Handle to spawn new futures
    pub fn new(
        source: &Network<T>,
        target: &Network<T>,
        rx: mpsc::UnboundedReceiver<Anchor>,
        handle: &reactor::Handle,
    ) -> Self {
        let handle = handle.clone();
        let source = source.clone();
        let target = target.clone();
        ProcessAnchors {
            source,
            target,
            stream: rx,
            handle,
        }
    }
}

impl<T: DuplexTransport + 'static> Future for ProcessAnchors<T> {
    type Item = ();
    type Error = ();
    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        loop {
            let anchor = try_ready!(self.stream.poll());
            match anchor {
                Some(a) => {
                    match self.source.flushed.read() {
                        Ok(lock) => {
                            if lock.is_some() {
                                return Ok(Async::Ready(()));
                            }
                        }
                        Err(e) => {
                            error!("error acquiring flush event lock: {:?}", e);
                            return Err(());
                        }
                    };
                    self.handle.spawn(a.process(&self.target));
                }
                None => {
                    return Ok(Async::Ready(()));
                }
            };
        }
    }
}

/// Stream of block headers on one chain to be posted to the other
pub struct WatchAnchors<T: DuplexTransport + 'static> {
    stream: Box<dyn Stream<Item = BlockHeader, Error = ()>>,
    header: Option<BlockHeader>,
    tx: mpsc::UnboundedSender<Anchor>,
    source: Network<T>,
    handle: reactor::Handle,
}

impl<T: DuplexTransport + 'static> WatchAnchors<T> {
    /// Returns a newly created WatchAnchors Future
    ///
    /// # Arguments
    ///
    /// * `source` - Network where the block headers are found
    /// * `handle` - Handle to spawn new futures
    pub fn new(source: &Network<T>, tx: mpsc::UnboundedSender<Anchor>, handle: &reactor::Handle) -> Self {
        let network_type = source.network_type;
        let h = handle.clone();
        let handle = handle.clone();
        let timeout = source.timeout;
        let flushed = source.flushed.clone();

        let block_stream = source
            .web3
            .eth_subscribe()
            .subscribe_new_heads()
            .flushed(&flushed)
            .timeout(timeout, &h)
            .map_err(move |e| {
                error!("error in anchor stream on {:?}: {:?}", network_type, e);
            });

        WatchAnchors {
            stream: Box::new(block_stream),
            header: None,
            tx,
            source: source.clone(),
            handle,
        }
    }

    fn process_header(&self, header: &BlockHeader) {
        header
            .number
            .map_or_else(|| self.no_block_number(), |number| self.spawn_to_anchor(number));
    }

    fn no_block_number(&self) {
        warn!("no block number in block head event on {:?}", self.source.network_type);
    }

    fn spawn_to_anchor(&self, block_number: U64) {
        let tx = self.tx.clone();
        let network_type = self.source.network_type;
        let web3 = self.source.web3.clone();
        let handle = self.handle.clone();
        let confirmations = self.source.confirmations;
        match block_number
            .checked_rem(self.source.anchor_frequency.into())
            .map(|u| u.low_u64())
        {
            Some(c) if c == self.source.confirmations => {
                let block_id = BlockId::Number(BlockNumber::Number(block_number - confirmations));

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
                                let block_number: U64 = b.number.unwrap();

                                let anchor = Anchor {
                                    block_hash,
                                    block_number,
                                };

                                info!("anchor block confirmed, anchoring on {:?}: {}", network_type, &anchor);

                                tx.unbounded_send(anchor).unwrap();
                                Ok(())
                            }
                            None => {
                                warn!("no block found for anchor confirmations on {:?}", network_type);
                                Ok(())
                            }
                        })
                        .or_else(move |e| {
                            error!("error waiting for anchor confirmations on {:?}: {:?}", network_type, e);
                            Ok(())
                        }),
                );
            }
            _ => (),
        };
    }
}

impl<T: DuplexTransport + 'static> Future for WatchAnchors<T> {
    type Item = ();
    type Error = ();
    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        loop {
            if let Some(ref blockheader) = self.header {
                self.process_header(blockheader);
                self.header = None;
            } else {
                let header_option = try_ready!(self.stream.poll());
                match header_option {
                    Some(header) => {
                        self.header = Some(header);
                    }
                    None => {
                        return Ok(Async::Ready(()));
                    }
                }
            }
        }
    }
}
