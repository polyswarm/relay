use relay::{Network, TransactionApprovalState};
use std::ops::Add;
use std::rc::Rc;
use std::time::{Duration, Instant};
use tokio_core::reactor;
use web3::api::SubscriptionStream;
use web3::futures::prelude::*;
use web3::types::H256;
use web3::{DuplexTransport, ErrorKind};

// From ethereum_types but not reexported by web3
pub fn clean_0x(s: &str) -> &str {
    if s.starts_with("0x") {
        &s[2..]
    } else {
        s
    }
}

/// TimeoutStream adds a timeout to an existing Stream.
/// returns Err if too much time has passed since the last object from the stream
pub struct TimeoutStream<I> {
    stream: Box<Stream<Item = I, Error = web3::Error>>,
    duration: Duration,
    timeout: reactor::Timeout,
}

impl<I> TimeoutStream<I> {
    /// Returns a newly created TimeoutStream Stream
    ///
    /// # Arguments
    ///
    /// * `stream` - Boxed stream to timeout
    /// * `duration` - Duration of time to trigger the timeout
    /// * `handle` - Handle to create a reactor::Timeout Future
    pub fn new(
        stream: Box<Stream<Item = I, Error = web3::Error>>,
        duration: Duration,
        handle: &reactor::Handle,
    ) -> Result<Self, ()> {
        let timeout = reactor::Timeout::new(duration, handle).map_err(move |e| {
            error!("error creating timeout: {:?}", e);
        })?;
        Ok(TimeoutStream {
            stream,
            duration,
            timeout,
        })
    }
}

impl<I> Stream for TimeoutStream<I> {
    type Item = I;
    type Error = web3::Error;

    /// Returns Items from the stream, or Err if timed out
    fn poll(&mut self) -> Poll<Option<Self::Item>, Self::Error> {
        match self.stream.poll() {
            Ok(Async::NotReady) => match self.timeout.poll() {
                Ok(Async::Ready(_)) => Err(web3::Error::from_kind(ErrorKind::Msg(
                    "Geth connection unavailable".to_string(),
                ))),
                Ok(Async::NotReady) => Ok(Async::NotReady),
                Err(_) => Err(web3::Error::from_kind(ErrorKind::Msg("Timeout broken".to_string()))),
            },
            Ok(Async::Ready(Some(msg))) => {
                let mut at = Instant::now();
                at = at.add(self.duration);
                self.timeout.reset(at);
                Ok(Async::Ready(Some(msg)))
            }
            Ok(Async::Ready(None)) => Ok(Async::Ready(None)),
            Err(e) => Err(e),
        }
    }
}

/// Trait to add to any Stream for creating a TimeoutStream via timeout()
pub trait Timeout<I> {
    ///Returns a TimeoutStream that wraps the existing stream
    ///
    /// # Arguments
    ///
    /// * `self` - Existing Stream that this is added to. Consumes self.
    /// * `duration` - Time in seconds to trigger a timeout
    /// # `handle` - Tokio reactor::Handle for Creating Timeout Future
    fn timeout(self, duration: u64, handle: &reactor::Handle) -> Result<TimeoutStream<I>, ()>;
}

/// Add Timeout trait to SubscribtionStream, which is returned by web3.eth_subscribe()
impl<T, I> Timeout<I> for SubscriptionStream<T, I>
where
    T: DuplexTransport + 'static,
    I: serde::de::DeserializeOwned + 'static,
{
    fn timeout(self, duration: u64, handle: &reactor::Handle) -> Result<TimeoutStream<I>, ()> {
        let timeout = Duration::from_secs(duration);
        let handle = handle.clone();
        TimeoutStream::new(Box::new(self), timeout, &handle)
    }
}

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
                    Some(TransactionApprovalState::Removed) => Ok(Async::Ready(None)),
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
