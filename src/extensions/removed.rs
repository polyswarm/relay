use web3::futures::prelude::*;
use web3::types::H256;
use web3::DuplexTransport;

use relay::{Network, TransferApprovalState};

pub struct ExitOnLogRemoved<T, I, E>
where
    T: DuplexTransport + 'static,
{
    target: Network<T>,
    tx_hash: H256,
    future: Box<dyn Future<Item = I, Error = E>>,
}

impl<T, I, E> ExitOnLogRemoved<T, I, E>
where
    T: DuplexTransport + 'static,
{
    pub fn new(target: &Network<T>, tx_hash: H256, future: Box<dyn Future<Item = I, Error = E>>) -> Self {
        ExitOnLogRemoved {
            target: target.clone(),
            tx_hash,
            future,
        }
    }
}

impl<T, I, E> Future for ExitOnLogRemoved<T, I, E>
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
pub trait CancelRemoved<T, I, E>
where
    T: DuplexTransport + 'static,
{
    ///Returns a TimeoutStream that wraps the existing stream
    ///
    /// # Arguments
    ///
    /// * `self` - Existing Future that this is added to. Consumes self.
    /// * `target` - Target network to check against
    /// * `tx_hash` - Tx hash to check for removal
    fn cancel_removed(self, target: &Network<T>, tx_hash: H256) -> ExitOnLogRemoved<T, I, E>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::transport::MockTransport;
    use relay::NetworkType;
    use std::time::Duration;
    use tokio_core::reactor;
    use web3::futures::try_ready;

    struct TestCheckRemoved {
        inner: Box<dyn Future<Item = (), Error = ()>>,
    }

    impl TestCheckRemoved {
        fn new(handle: &reactor::Handle) -> Self {
            TestCheckRemoved {
                inner: Box::new(
                    reactor::Timeout::new(Duration::from_secs(1), handle)
                        .unwrap()
                        .map_err(|_| {}),
                ),
            }
        }
    }

    impl Future for TestCheckRemoved {
        type Item = ();
        type Error = ();

        fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
            try_ready!(self.inner.poll());
            Ok(Async::Ready(()))
        }
    }

    impl<T> CancelRemoved<T, (), ()> for TestCheckRemoved
    where
        T: DuplexTransport + 'static,
    {
        fn cancel_removed(self, target: &Network<T>, tx_hash: H256) -> ExitOnLogRemoved<T, (), ()> {
            ExitOnLogRemoved::<T, (), ()>::new(target, tx_hash, Box::new(self))
        }
    }

    #[test]
    fn check_log_removed_future_should_return_none_if_removed() {
        // arrange
        let mut eloop = tokio_core::reactor::Core::new().unwrap();
        let handle = eloop.handle();
        let mock = MockTransport::new();
        let target = mock.new_network(NetworkType::Home).unwrap();
        let future = ExitOnLogRemoved::<MockTransport, (), std::io::Error>::new(
            &target,
            H256::zero(),
            Box::new(reactor::Timeout::new(Duration::from_secs(1), &handle).unwrap()),
        );
        target
            .pending
            .write()
            .unwrap()
            .put(H256::zero(), TransferApprovalState::Removed);
        // act
        let result = eloop.run(future).unwrap();
        // assert
        assert_eq!(result, None)
    }

    #[test]
    fn check_log_removed_future_should_return_some_if_not_removed() {
        // arrange
        let mut eloop = tokio_core::reactor::Core::new().unwrap();
        let handle = eloop.handle();
        let mock = MockTransport::new();
        let target = mock.new_network(NetworkType::Home).unwrap();
        let future = ExitOnLogRemoved::<MockTransport, (), std::io::Error>::new(
            &target,
            H256::zero(),
            Box::new(reactor::Timeout::new(Duration::from_secs(1), &handle).unwrap()),
        );
        // act
        let result = eloop.run(future).unwrap();
        // assert
        assert_eq!(result, Some(()))
    }

    #[test]
    fn check_log_removed_impl_should_return_none_if_not_removed() {
        // arrange
        let mut eloop = tokio_core::reactor::Core::new().unwrap();
        let handle = eloop.handle();
        let mock = MockTransport::new();
        let target = mock.new_network(NetworkType::Home).unwrap();
        let future = TestCheckRemoved::new(&handle).cancel_removed(&target, H256::zero());
        // act
        target
            .pending
            .write()
            .unwrap()
            .put(H256::zero(), TransferApprovalState::Removed);
        let result = eloop.run(future);
        // assert
        assert_eq!(result, Ok(None))
    }

    #[test]
    fn check_log_removed_impl_should_return_some_if_not_removed() {
        // arrange
        let mut eloop = tokio_core::reactor::Core::new().unwrap();
        let handle = eloop.handle();
        let mock = MockTransport::new();
        let target = mock.new_network(NetworkType::Home).unwrap();
        let future = TestCheckRemoved::new(&handle).cancel_removed(&target, H256::zero());

        // act
        let result = eloop.run(future);
        // assert
        assert_eq!(result, Ok(Some(())))
    }

    #[test]
    fn check_log_removed_impl_should_forward_err() {
        // arrange
        let mut eloop = tokio_core::reactor::Core::new().unwrap();
        let mock = MockTransport::new();
        let target = mock.new_network(NetworkType::Home).unwrap();
        let future = TestCheckRemoved {
            inner: Box::new(web3::futures::future::err(())),
        }
        .cancel_removed(&target, H256::zero());

        // act
        let result = eloop.run(future);
        // assert
        assert_eq!(result, Err(()))
    }
}
