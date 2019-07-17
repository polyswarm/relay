use std::rc::Rc;
use web3::futures::prelude::*;
use web3::types::H256;
use web3::DuplexTransport;

use relay::{Network, TransferApprovalState};

pub struct CheckLogRemoved<T, I, E>
where
    T: DuplexTransport + 'static,
{
    target: Rc<Network<T>>,
    tx_hash: H256,
    future: Box<Future<Item = I, Error = E>>,
}

impl<T, I, E> CheckLogRemoved<T, I, E>
where
    T: DuplexTransport + 'static,
{
    pub fn new(target: Rc<Network<T>>, tx_hash: H256, future: Box<Future<Item = I, Error = E>>) -> Self {
        CheckLogRemoved {
            target,
            tx_hash,
            future,
        }
    }
}

impl<T, I, E> Future for CheckLogRemoved<T, I, E>
where
    T: DuplexTransport + 'static,
{
    type Item = Option<I>;
    type Error = E;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        match self.future.poll() {
            Ok(Async::Ready(result)) => Ok(Async::Ready(Some(result))),
            Ok(Async::NotReady) => {
                // Check removed status
                match self.target.pending.read().unwrap().peek(&self.tx_hash) {
                    Some(TransferApprovalState::Removed) => Ok(Async::Ready(None)),
                    _ => Ok(Async::NotReady),
                }
            }
            Err(e) => Err(e),
        }
    }
}

/// Trait to add to any Stream for creating a CheckLogRemoved via check_log_removed()
pub trait CheckRemoved<T, I, E>
where
    T: DuplexTransport + 'static,
{
    ///Returns a TimeoutStream that wraps the existing stream
    ///
    /// # Arguments
    ///
    /// * `self` - Existing Stream that this is added to. Consumes self.
    /// * `target` - Target network to check against
    /// * `tx_hash` - Tx hash to check for removal
    fn check_log_removed(self, target: &Rc<Network<T>>, tx_hash: H256) -> CheckLogRemoved<T, I, E>;
}
