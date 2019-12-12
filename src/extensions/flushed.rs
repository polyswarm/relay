use serde;
use std::sync::{Arc, RwLock};
use web3::api::{SubscriptionResult, SubscriptionStream};
use web3::futures::prelude::*;
use web3::futures::try_ready;
use web3::DuplexTransport;

use crate::eth::Event;

/// FlushedStream adds a flush check to an existing Stream.
/// It exits and calls unsubscribe on original stream
pub struct FlushedStream<T, I>
where
    T: DuplexTransport + 'static,
    I: serde::de::DeserializeOwned + 'static,
{
    flushed: Arc<RwLock<Option<Event>>>,
    subscribe: SubscriptionResult<T, I>,
    stream: Option<SubscriptionStream<T, I>>,
    unsubscribe: Option<Box<dyn Future<Item = bool, Error = web3::Error>>>,
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
    pub fn new(flushed: &Arc<RwLock<Option<Event>>>, subscribe: SubscriptionResult<T, I>) -> Self {
        FlushedStream {
            flushed: flushed.clone(),
            subscribe,
            stream: None,
            unsubscribe: None,
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

    /// Returns an item from the stream, or None if flushed
    fn poll(&mut self) -> Poll<Option<Self::Item>, Self::Error> {
        let flushed = self.flushed.clone();
        loop {
            // Trying to unsubscribe
            if let Some(future) = &mut self.unsubscribe {
                try_ready!(future.poll());
                return Ok(Async::Ready(None));
            }

            // Have an open subscription
            if let Some(stream) = &mut self.stream {
                let lock = flushed.read().map_err(|e| {
                    error!("error acquiring flush event lock: {:?}", e);
                    web3::Error::Internal
                })?;
                if lock.is_some() {
                    let stream = self.stream.take().unwrap();
                    self.unsubscribe = Some(Box::new(stream.unsubscribe()));
                    continue;
                } else {
                    let message = try_ready!(stream.poll());
                    return Ok(Async::Ready(message));
                }
            }

            // Opening the subscription
            let stream = try_ready!(self.subscribe.poll());
            self.stream = Some(stream);
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
        FlushedStream::new(flushed, self)
    }
}
