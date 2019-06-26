use std::ops::Add;
use std::time::{Duration, Instant};
use tokio_core::reactor;
use web3::api::SubscriptionStream;
use web3::futures::prelude::*;
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
