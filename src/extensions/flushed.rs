use serde;
use std::sync::{Arc, RwLock};
use web3::api::{SubscriptionResult, SubscriptionStream};
use web3::error::Error;
use web3::futures::prelude::*;
use web3::futures::try_ready;
use web3::DuplexTransport;

use crate::eth::Event;

/// Enum for the two stages of subscribing to a timeout stream
/// Subscribing holds a future that returns a TimeoutStream
/// Subscribed holds a TimeoutStream
pub enum SubscriptionState<T, I>
where
    T: DuplexTransport + 'static,
    I: serde::de::DeserializeOwned + 'static,
{
    Subscribing(SubscriptionResult<T, I>),
    Subscribed(SubscriptionStream<T, I>),
}

/// FlushedStream adds a flush check to an existing Stream.
/// It exists and unsubscribes
pub struct FlushedStream<T, I>
where
    T: DuplexTransport + 'static,
    I: serde::de::DeserializeOwned + 'static,
{
    flushed: Arc<RwLock<Option<Event>>>,
    state: SubscriptionState<T, I>,
}

impl<T, I> FlushedStream<T, I>
where
    T: DuplexTransport + 'static,
    I: serde::de::DeserializeOwned + 'static,
{
    /// Returns a newly created FlushedStream Stream
    ///
    /// # Arguments
    ///
    /// * `flushed` - Event marking the flush
    /// * `state` - SubscriptionResult from eth_subscribe()
    pub fn new(flushed: &Arc<RwLock<Option<Event>>>, state: SubscriptionState<T, I>) -> Self {
        FlushedStream {
            flushed: flushed.clone(),
            state,
        }
    }
}

impl<T, I> Stream for FlushedStream<T, I>
where
    T: DuplexTransport + 'static,
    I: serde::de::DeserializeOwned + 'static,
{
    type Item = I;
    type Error = web3::Error;

    /// Returns Items from the stream, or Err if timed out
    fn poll(&mut self) -> Poll<Option<Self::Item>, Self::Error> {
        let flushed = self.flushed.clone();
        loop {
            let next = match self.state {
                SubscriptionState::Subscribing(ref mut future) => {
                    let stream = try_ready!(future.poll());
                    Some(SubscriptionState::Subscribed(stream))
                }
                SubscriptionState::Subscribed(ref mut stream) => match stream.poll() {
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
                    },
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
                },
            };
            // If the Future finished, set the state to subscribed
            if let Some(next_state) = next {
                self.state = next_state;
            }
        }
    }
}

/// Trait to add to any Stream for creating a FlushedStream via timeout()
pub trait Flushed<T, I>
where
    T: DuplexTransport + 'static,
    I: serde::de::DeserializeOwned + 'static,
{
    ///Returns a FlushedStream that wraps the existing stream
    ///
    /// # Arguments
    ///
    /// * `self` - Existing Stream that this is added to. Consumes self.
    /// * `flsuhed` - Event that triggered flush
    fn flushed(self, flushed: &Arc<RwLock<Option<Event>>>) -> FlushedStream<T, I>;
}

/// Add Timeout trait to SubscriptionResult, which is returned by web3.eth_subscribe()
impl<T, I> Flushed<T, I> for SubscriptionResult<T, I>
where
    T: DuplexTransport + 'static,
    I: serde::de::DeserializeOwned + 'static,
{
    fn flushed(self, flushed: &Arc<RwLock<Option<Event>>>) -> FlushedStream<T, I> {
        FlushedStream::new(flushed, SubscriptionState::Subscribing(self))
    }
}
