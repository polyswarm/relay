use super::relay::Network;
use super::utils::{build_transaction, get_store_for_keyfiles};
use rlp::RlpStream;
use std::fmt;
use std::rc::Rc;
use std::sync::atomic::Ordering;
use tokio_core::reactor;
use web3;
use web3::contract::Options;
use web3::futures::prelude::*;
use web3::futures::sync::mpsc;
use web3::futures::try_ready;
use web3::types::{BlockId, BlockNumber, TransactionReceipt, H256, U256};
use web3::DuplexTransport;

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
    fn process<T: DuplexTransport + 'static>(&self, target: &Rc<Network<T>>) -> ProcessAnchor {
        ProcessAnchor::new(self, target)
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
            if let Some(a) = anchor {
                self.handle.spawn(a.process(&self.target))
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

/// Future that transacts with the ERC20Relay contract to permanently anchor a block
pub struct ProcessAnchor(Box<Future<Item = TransactionReceipt, Error = web3::Error>>);

impl ProcessAnchor {
    /// Returns a newly created ProcessAnchor Future
    ///
    /// # Arguments
    ///
    /// * `target` - Network where the block headers are anchored
    /// * `handle` - Handle to spawn new futures
    pub fn new<T: DuplexTransport + 'static>(anchor: &Anchor, target: &Rc<Network<T>>) -> Self {
        info!("anchoring block {}", anchor);
        let anchor = *anchor;
        let target = target.clone();
        let store = get_store_for_keyfiles(&target.keydir);
        let mut s = RlpStream::new();
        let fn_data = target
            .relay
            .get_function_data("anchor", (anchor.block_hash, anchor.block_number))
            .unwrap();
        let options = Options::with(|options| {
            options.gas = Some(target.get_gas_limit());
            options.gas_price = Some(target.get_gas_price());
            options.value = Some(0.into());
            options.nonce = Some(U256::from(target.nonce.load(Ordering::SeqCst)));
            target.nonce.fetch_add(1, Ordering::SeqCst);
        });
        build_transaction(
            &mut s,
            &fn_data,
            &target.relay.address(),
            &target.account,
            &store,
            &options,
            &target.password,
            target.chain_id,
        );
        let future = target
            .relay
            .send_raw_call_with_confirmations(s.as_raw().into(), target.confirmations as usize);
        ProcessAnchor(Box::new(future))
    }
}

impl Future for ProcessAnchor {
    type Item = ();
    type Error = ();
    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        // Not using try_ready so the result can be logged easily
        match self.0.poll() {
            Ok(Async::Ready(receipt)) => {
                info!("anchor processed: {:?}", receipt);
                Ok(Async::Ready(()))
            }
            Ok(Async::NotReady) => Ok(Async::NotReady),
            Err(e) => {
                error!("error anchoring block: {:?}", e);
                Err(())
            }
        }
    }
}
