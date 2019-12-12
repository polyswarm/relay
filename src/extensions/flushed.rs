use serde;
use std::sync::{Arc, RwLock};
use web3::api::{SubscriptionResult, SubscriptionStream};
use web3::error::Error;
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
    subscribe_future: SubscriptionResult<T, I>,
    subscription_stream: Option<SubscriptionStream<T, I>>,
    unsubscribe_future: Option<Box<dyn Future<Item = bool, Error = web3::Error>>>,
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
    pub fn new(flushed: &Arc<RwLock<Option<Event>>>, state: SubscriptionResult<T, I>) -> Self {
        FlushedStream {
            flushed: flushed.clone(),
            subscribe_future: state,
            subscription_stream: None,
            unsubscribe_future: None,
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
            if let Some(future) = &mut self.unsubscribe_future {
                try_ready!(future.poll());
                return Ok(Async::Ready(None));
            } else if let Some(stream) = &mut self.subscription_stream {
                match flushed.read() {
                    Ok(lock) => {
                        if lock.is_some() {
                            // End stream if flushed
                            let stream = self.subscription_stream.take().unwrap();
                            self.unsubscribe_future = Some(Box::new(stream.unsubscribe()));
                        } else {
                            let message = try_ready!(stream.poll());
                            return Ok(Async::Ready(message));
                        }
                    }
                    Err(e) => {
                        error!("error acquiring flush event lock: {:?}", e);
                        return Err(Error::Internal);
                    }
                }
            } else {
                let stream = try_ready!(self.subscribe_future.poll());
                self.subscription_stream = Some(stream);
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
        FlushedStream::new(flushed, self)
    }
}
