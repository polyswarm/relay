use web3::contract::Options;
use web3::futures::prelude::*;
use web3::futures::try_ready;
use web3::types::{BlockNumber, FilterBuilder, Log, TransactionReceipt, U256, U64};

use crate::eth::contracts::FLUSH_EVENT_SIGNATURE;
use crate::eth::Event;
use crate::network::Network;
use web3::DuplexTransport;

enum CheckForPastFlushState {
    CheckFlushBlock(Box<dyn Future<Item = U256, Error = ()>>),
    GetFlushLog(Box<dyn Future<Item = Vec<Log>, Error = ()>>),
    GetFlushReceipt(Box<Log>, Box<dyn Future<Item = Option<TransactionReceipt>, Error = ()>>),
}

/// Future for checking if the flush block is set in relay, then getting the transaction receipt if so
pub struct CheckForPastFlush<T: DuplexTransport + 'static> {
    state: CheckForPastFlushState,
    source: Network<T>,
}

impl<T: DuplexTransport + 'static> CheckForPastFlush<T> {
    /// Create a new CheckForPastFlush Future
    /// # Arguments
    ///
    /// * `source` - Network being flushed
    /// * `tx` - Sender to trigger Flush event processor
    pub fn new(source: &Network<T>) -> Self {
        let flush_block_future = source
            .relay
            .query("flushBlock", (), None, Options::default(), BlockNumber::Latest)
            .map_err(|e| {
                error!("error retrieving flush block: {:?}", e);
            });
        CheckForPastFlush {
            state: CheckForPastFlushState::CheckFlushBlock(Box::new(flush_block_future)),
            source: source.clone(),
        }
    }
    /// Create a Future to get the flush even logs given the block the event occurred at
    /// # Arguments
    ///
    /// * `block` - Block number for the flush event
    pub fn get_flush_log(&self, block_number: U64) -> Box<dyn Future<Item = Vec<Log>, Error = ()>> {
        // Not sure if there is a better way to get the log we want, probably get block or something
        let filter = FilterBuilder::default()
            .address(vec![self.source.relay.address()])
            .from_block(BlockNumber::from(block_number - 1))
            .to_block(BlockNumber::Number(block_number + 1))
            .topics(Some(vec![FLUSH_EVENT_SIGNATURE.into()]), None, None, None)
            .build();
        Box::new(self.source.web3.eth().logs(filter).map_err(|e| {
            error!("error getting flush log: {:?}", e);
        }))
    }
}

impl<T: DuplexTransport + 'static> Future for CheckForPastFlush<T> {
    type Item = Option<Event>;
    type Error = ();
    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        loop {
            let source = self.source.clone();
            let next = match self.state {
                CheckForPastFlushState::CheckFlushBlock(ref mut future) => {
                    let block = try_ready!(future.poll());
                    let block_number = block.as_u64().into();
                    info!("flush block on start is {}", block_number);
                    if block_number > U64::zero() {
                        let future = self.get_flush_log(block_number);
                        CheckForPastFlushState::GetFlushLog(future)
                    } else {
                        return Ok(Async::Ready(None));
                    }
                }
                CheckForPastFlushState::GetFlushLog(ref mut stream) => {
                    let logs = try_ready!(stream.poll());
                    if !logs.is_empty() {
                        let log = &logs[0];
                        let removed = log.removed.unwrap_or(false);
                        match log.transaction_hash {
                            Some(tx_hash) => {
                                let future = source.get_receipt(removed, tx_hash);
                                CheckForPastFlushState::GetFlushReceipt(Box::new(log.clone()), Box::new(future))
                            }
                            None => {
                                error!("flush log missing transaction hash");
                                return Err(());
                            }
                        }
                    } else {
                        error!("Didn't find any flush event at flushBlock");
                        return Ok(Async::Ready(None));
                    }
                }
                CheckForPastFlushState::GetFlushReceipt(ref log, ref mut future) => {
                    let receipt_option = try_ready!(future.poll());
                    match receipt_option {
                        Some(receipt) => {
                            return Ok(Async::Ready(Some(Event::new(&*log.clone(), &receipt))));
                        }
                        None => {
                            error!("error getting flush receipt");
                            return Err(());
                        }
                    }
                }
            };
            self.state = next;
        }
    }
}
