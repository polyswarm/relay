use std::process;
use failure::Error;
use tokio_core::reactor;
use web3::futures::future::{err, Either};
use web3::futures::sync::mpsc;
use web3::futures::Future;
use web3::DuplexTransport;

use crate::relay::network::Network;
use crate::server::{RequestType, HandleRequests};


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

    fn handle_requests(
        homechain: &Network<T>,
        sidechain: &Network<T>,
        rx: mpsc::UnboundedReceiver<RequestType>,
        handle: &reactor::Handle,
    ) -> HandleRequests<T> {
        HandleRequests::new(homechain, sidechain, rx, handle)
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
        let sidechain = self.sidechain.clone();
        let homechain = self.homechain.clone();
        let handle = handle.clone();
        sidechain
            .check_flush_block()
            .and_then(move |flush_option| {
                if let Ok(mut lock) = sidechain.flushed.write() {
                    *lock = flush_option.clone();
                } else {
                    error!("error getting lock on startup");
                    return Either::B(err(()));
                }

                let (watch_anchors, process_anchors) = sidechain.handle_anchors(&homechain, &handle);
                let (watch_side_past, process_side_past) = sidechain.recheck_past_transfer_logs(&homechain, &handle);
                let (watch_home_past, process_home_past) = homechain.recheck_past_transfer_logs(&sidechain, &handle);

                Either::A(
                    watch_anchors
                        .join(process_anchors)
                        .join(watch_side_past)
                        .join(process_side_past)
                        .join(watch_home_past)
                        .join(process_home_past)
                        .join(homechain.watch_transfer_logs(&sidechain, &handle))
                        .join(sidechain.watch_transfer_logs(&homechain, &handle))
                        .join(sidechain.watch_flush_logs(&homechain, flush_option, &handle))
                        .join(Relay::handle_requests(&homechain, &sidechain, rx, &handle))
                        .and_then(|_| Ok(())),
                )
            })
            .map_err(|e| {
                error!("error at top level: {:?}", e);
                process::exit(-1);
            })
    }
}
