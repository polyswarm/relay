use serde;
use std::ops::Add;
use std::time::{Duration, Instant};
use tokio_core::reactor;
use web3::error::Error;
use web3::futures::prelude::*;
use web3::DuplexTransport;

use crate::extensions::flushed::FlushedStream;

/// TimeoutStream adds a timeout to an existing Stream.
/// returns Err if too much time has passed since the last object from the stream
pub struct TimeoutStream<I>
where
    I: serde::de::DeserializeOwned + 'static,
{
    stream: Box<dyn Stream<Item = I, Error = web3::Error>>,
    duration: Duration,
    timeout: reactor::Timeout,
}

impl<I> TimeoutStream<I>
where
    I: serde::de::DeserializeOwned + 'static,
{
    /// Returns a newly created TimeoutStream Stream
    ///
    /// # Arguments
    ///
    /// * `stream` - Boxed stream to timeout
    /// * `duration` - Duration of time to trigger the timeout
    /// * `handle` - Handle to create a reactor::Timeout Future
    pub fn new(
        stream: Box<dyn Stream<Item = I, Error = web3::Error>>,
        duration: Duration,
        handle: &reactor::Handle,
    ) -> Self {
        let timeout = reactor::Timeout::new(duration, handle).expect("error creating timeout");
        TimeoutStream {
            stream,
            duration,
            timeout,
        }
    }

    /// Reset the given timeout
    ///
    /// # Arguments
    ///
    /// * `timeout` - reactor::Timeout that you want to be reset
    /// * `duration` - Duration of the timeout
    pub fn reset_timeout(timeout: &mut reactor::Timeout, duration: &Duration) {
        let mut at = Instant::now();
        at = at.add(duration.clone());
        timeout.reset(at);
    }
}

impl<I> Stream for TimeoutStream<I>
where
    I: serde::de::DeserializeOwned + 'static,
{
    type Item = I;
    type Error = web3::Error;

    /// Returns Items from the stream, or Err if timed out
    fn poll(&mut self) -> Poll<Option<Self::Item>, Self::Error> {
        match self.stream.poll() {
            // If the stream does not have the next element, check the timeout
            Ok(Async::NotReady) => match self.timeout.poll() {
                // If the timeout is triggered, error out
                Ok(Async::Ready(_)) => {
                    Err(Error::Unreachable)
                }
                // If timeout not triggered, return NotReady
                Ok(Async::NotReady) => {
                    Ok(Async::NotReady)
                }
                // If timeout errors out, return error
                Err(_) => {
                    error!("Timeout broken");
                    Err(Error::Internal)
                }
            },
            // If the stream returns an item, reset timeout and return the item
            Ok(Async::Ready(Some(msg))) => {
                TimeoutStream::<I>::reset_timeout(&mut self.timeout, &self.duration);
                Ok(Async::Ready(Some(msg)))
            }
            // Forward stream done
            Ok(Async::Ready(None)) => {
                Ok(Async::Ready(None))
            }
            // Forward errors
            Err(e) => {
                Err(e)
            }
        }
    }
}

/// Trait to add to any Stream for creating a TimeoutStream via timeout()
pub trait Timeout<I>
where
    I: serde::de::DeserializeOwned + 'static,
{
    ///Returns a TimeoutStream that wraps the existing stream
    ///
    /// # Arguments
    ///
    /// * `self` - Existing Stream that this is added to. Consumes self.
    /// * `duration` - Time in seconds to trigger a timeout
    /// # `handle` - Tokio reactor::Handle for Creating Timeout Future
    fn timeout(self, duration: u64, handle: &reactor::Handle) -> TimeoutStream<I>;
}

/// Add Timeout trait to SubscriptionResult, which is returned by web3.eth_subscribe()
impl<T, I> Timeout<I> for FlushedStream<T, I>
where
    T: DuplexTransport + 'static,
    I: serde::de::DeserializeOwned + 'static,
{
    fn timeout(self, duration: u64, handle: &reactor::Handle) -> TimeoutStream<I> {
        let timeout = Duration::from_secs(duration);
        let handle = handle.clone();
        TimeoutStream::new(Box::new(self), timeout, &handle)
    }
}
