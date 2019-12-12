use serde;
use std::sync::{Arc, RwLock};
use web3::error::Error;
use web3::futures::prelude::*;
use web3::DuplexTransport;
use web3::api::SubscriptionStream;

use super::timeout::TimeoutStream;
use crate::transfers::live::Event;

/// FlushedStream adds a timeout to an existing Stream.
/// returns Err if too much time has passed since the last object from the stream
pub struct FlushedStream<I>
where
    I: serde::de::DeserializeOwned + 'static,
{
    flushed: Arc<RwLock<Option<Event>>>,
    stream: Box<dyn Stream<Item = I, Error=web3::Error>>,
}

impl<I> FlushedStream<I>
where
    I: serde::de::DeserializeOwned + 'static,
{
    /// Returns a newly created FlushedStream Stream
    ///
    /// # Arguments
    ///
    /// * `stream` - Boxed stream to timeout
    /// * `duration` - Duration of time to trigger the timeout
    /// * `handle` - Handle to create a reactor::Timeout Future
    pub fn new(flushed: &Arc<RwLock<Option<Event>>>, stream: Box<dyn Stream<Item = I, Error=web3::Error>>) -> Self {
        FlushedStream {
            flushed: flushed.clone(),
            stream
        }
    }
}

impl<I> Stream for FlushedStream<I>
where
    I: serde::de::DeserializeOwned + 'static,
{
    type Item = I;
    type Error = web3::Error;

    /// Returns Items from the stream, or Err if timed out
    fn poll(&mut self) -> Poll<Option<Self::Item>, Self::Error> {
        loop {
            let flushed = self.flushed.clone();
            match self.stream.poll() {
                // If the stream does not have the next element, check the lock
                Ok(Async::NotReady) => match flushed.read() {
                    Ok(lock) => {
                        if lock.is_some() {
                            // End stream if locked
                            return Ok(Async::Ready(None));
                        } else {
                            return Ok(Async::NotReady);
                        }
                    }
                    Err(e) => {
                        error!("error acquiring flush event lock: {:?}", e);
                        return Err(Error::Internal);
                    }
                }
                Ok(Async::Ready(Some(msg))) => {
                    return Ok(Async::Ready(Some(msg)));
                }
                // Forward stream done
                Ok(Async::Ready(None)) => {
                    return Ok(Async::Ready(None));
                }
                // Forward errors
                Err(e) => {
                    return Err(e);
                }
            };
        }
    }
}

/// Trait to add to any Stream for creating a FlushedStream via timeout()
pub trait Flushed<I>
where
    I: serde::de::DeserializeOwned + 'static,
{
    ///Returns a FlushedStream that wraps the existing stream
    ///
    /// # Arguments
    ///
    /// * `self` - Existing Stream that this is added to. Consumes self.
    /// * `duration` - Time in seconds to trigger a timeout
    /// # `handle` - Tokio reactor::Handle for Creating Timeout Future
    fn flushed(self, flushed: &Arc<RwLock<Option<Event>>>) -> FlushedStream<I>;
}

impl<T, I> Flushed<I> for TimeoutStream<T, I>
where
    T: DuplexTransport + 'static,
    I: serde::de::DeserializeOwned + 'static,
{
    fn flushed(self, flushed: &Arc<RwLock<Option<Event>>>) -> FlushedStream<I> {
        FlushedStream::new(flushed, Box::new(self))
    }
}

impl<T, I> Flushed<I> for SubscriptionStream<T, I>
where
    T: DuplexTransport + 'static,
    I: serde::de::DeserializeOwned + 'static,
{
    fn flushed(self, flushed: &Arc<RwLock<Option<Event>>>) -> FlushedStream<I> {
        FlushedStream::new(flushed, Box::new(self))
    }
}

