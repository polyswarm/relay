use std::ops::Add;
use std::time::{Duration, Instant};
use tokio_core::reactor;
use web3::api::{SubscriptionResult, SubscriptionStream};
use web3::error::Error;
use web3::futures::prelude::*;
use web3::futures::try_ready;
use web3::DuplexTransport;

/// Enum for the two stages of subscribing to a timeout stream
/// Subscribing holds a future that returns a TimeoutStream
/// Subscribed holds a TimeoutStream
pub enum SubscriptionState<T, I>
where
    T: DuplexTransport + 'static,
    I: serde::de::DeserializeOwned + 'static,
{
    Subscribing(Box<dyn Future<Item = SubscriptionStream<T, I>, Error = web3::Error>>),
    Subscribed(SubscriptionStream<T, I>),
}

/// TimeoutStream adds a timeout to an existing Stream.
/// returns Err if too much time has passed since the last object from the stream
pub struct TimeoutStream<T, I>
where
    T: DuplexTransport + 'static,
    I: serde::de::DeserializeOwned + 'static,
{
    state: SubscriptionState<T, I>,
    duration: Duration,
    timeout: reactor::Timeout,
}

impl<T, I> TimeoutStream<T, I>
where
    T: DuplexTransport + 'static,
    I: serde::de::DeserializeOwned + 'static,
{
    /// Returns a newly created TimeoutStream Stream
    ///
    /// # Arguments
    ///
    /// * `stream` - Boxed stream to timeout
    /// * `duration` - Duration of time to trigger the timeout
    /// * `handle` - Handle to create a reactor::Timeout Future
    pub fn new(state: SubscriptionState<T, I>, duration: Duration, handle: &reactor::Handle) -> Self {
        let timeout = reactor::Timeout::new(duration, handle).expect("error creating timeout");
        TimeoutStream {
            state,
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

impl<T, I> Stream for TimeoutStream<T, I>
where
    T: DuplexTransport + 'static,
    I: serde::de::DeserializeOwned + 'static,
{
    type Item = I;
    type Error = web3::Error;

    /// Returns Items from the stream, or Err if timed out
    fn poll(&mut self) -> Poll<Option<Self::Item>, Self::Error> {
        loop {
            let next = match self.state {
                SubscriptionState::Subscribing(ref mut future) => {
                    let stream = try_ready!(future.poll());
                    Some(SubscriptionState::Subscribed(stream))
                }
                SubscriptionState::Subscribed(ref mut stream) => {
                    match stream.poll() {
                        // If the stream does not have the next element, check the timeout
                        Ok(Async::NotReady) => match self.timeout.poll() {
                            // If the timeout is triggered, error out
                            Ok(Async::Ready(_)) => {
                                return Err(Error::Transport("Ethereum connection unavailable".to_string()));
                            }
                            // If timeout not triggered, return NotReady
                            Ok(Async::NotReady) => {
                                return Ok(Async::NotReady);
                            }
                            // If timeout errors out, return error
                            Err(_) => {
                                return Err(Error::Transport("Timeout broken".to_string()));
                            }
                        },
                        // If the stream returns an item, reset timeout and return the item
                        Ok(Async::Ready(Some(msg))) => {
                            TimeoutStream::<T, I>::reset_timeout(&mut self.timeout, &self.duration);
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
            };
            // If the Future finished, set the state to subscribed
            if let Some(next_state) = next {
                self.state = next_state;
                TimeoutStream::<T, I>::reset_timeout(&mut self.timeout, &self.duration);
            }
        }
    }
}

/// Trait to add to any Stream for creating a TimeoutStream via timeout()
pub trait Timeout<T, I>
where
    T: DuplexTransport + 'static,
    I: serde::de::DeserializeOwned + 'static,
{
    ///Returns a TimeoutStream that wraps the existing stream
    ///
    /// # Arguments
    ///
    /// * `self` - Existing Stream that this is added to. Consumes self.
    /// * `duration` - Time in seconds to trigger a timeout
    /// # `handle` - Tokio reactor::Handle for Creating Timeout Future
    fn timeout(self, duration: u64, handle: &reactor::Handle) -> TimeoutStream<T, I>;
}

/// Add Timeout trait to SubscriptionResult, which is returned by web3.eth_subscribe()
impl<T, I> Timeout<T, I> for SubscriptionResult<T, I>
where
    T: DuplexTransport + 'static,
    I: serde::de::DeserializeOwned + 'static,
{
    fn timeout(self, duration: u64, handle: &reactor::Handle) -> TimeoutStream<T, I> {
        let timeout = Duration::from_secs(duration);
        let handle = handle.clone();
        TimeoutStream::new(SubscriptionState::Subscribing(Box::new(self)), timeout, &handle)
    }
}
