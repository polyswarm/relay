use std::rc::Rc;
use tokio_core::reactor;
use web3::{DuplexTransport, Web3};
use web3::contract::Contract;
use web3::futures::{Future, Stream};
use web3::futures::sync::mpsc;
use web3::types::{Address, FilterBuilder, H256, U256};

use super::contracts::{ERC20_ABI, ERC20_RELAY_ABI, TRANSFER_EVENT_SIGNATURE};
use super::errors::*;

type Task = Box<Future<Item = (), Error = ()>>;

// From ethereum_types but not reexported by web3
fn clean_0x(s: &str) -> &str {
    if s.starts_with("0x") {
        &s[2..]
    } else {
        s
    }
}

#[derive(Debug, Clone)]
pub struct Relay<T: DuplexTransport> {
    homechain: Rc<Network<T>>,
    sidechain: Rc<Network<T>>,
}

impl<T: DuplexTransport + 'static> Relay<T> {
    pub fn new(homechain: Network<T>, sidechain: Network<T>) -> Self {
        Self {
            homechain: Rc::new(homechain),
            sidechain: Rc::new(sidechain),
        }
    }

    fn transfer_future(
        chain_a: Rc<Network<T>>,
        chain_b: Rc<Network<T>>,
        handle: &reactor::Handle,
    ) -> Task {
        Box::new({
            chain_a
                .transfer_stream(handle)
                .for_each(move |transfer| {
                    chain_b.process_withdrawal(&transfer);
                    Ok(())
                })
                .map_err(move |e| {
                    error!(
                        "error processing withdrawal from {:?}: {:?}",
                        chain_a.network_type(),
                        e
                    )
                })
        })
    }

    fn anchor_future(
        homechain: Rc<Network<T>>,
        sidechain: Rc<Network<T>>,
        handle: &reactor::Handle,
    ) -> Task {
        Box::new({
            sidechain
                .anchor_stream(handle)
                .for_each(move |anchor| {
                    homechain.anchor(&anchor);
                    Ok(())
                })
                .map_err(move |e| {
                    error!(
                        "error anchoring from {:?}: {:?}",
                        sidechain.network_type(),
                        e
                    )
                })
        })
    }

    pub fn listen(&self, handle: &reactor::Handle) -> Task {
        Box::new(
            Self::anchor_future(self.homechain.clone(), self.sidechain.clone(), handle)
                .join(
                    Self::transfer_future(self.homechain.clone(), self.sidechain.clone(), handle)
                        .join(Self::transfer_future(
                            self.sidechain.clone(),
                            self.homechain.clone(),
                            handle,
                        )),
                )
                .and_then(|_| Ok(())),
        )
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct Transfer {
    tx_hash: H256,
    destination: Address,
    amount: U256,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct Anchor {
    block_number: U256,
    block_hash: H256,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum NetworkType {
    Home,
    Side,
}

#[derive(Debug)]
pub struct Network<T: DuplexTransport> {
    network_type: NetworkType,
    web3: Web3<T>,
    token: Contract<T>,
    relay: Contract<T>,
}

impl<T: DuplexTransport + 'static> Network<T> {
    pub fn new(network_type: NetworkType, transport: T, token: &str, relay: &str) -> Result<Self> {
        let web3 = Web3::new(transport);
        let token_address: Address = clean_0x(token)
            .parse()
            .chain_err(|| ErrorKind::InvalidAddress(token.to_owned()))?;
        let relay_address: Address = clean_0x(relay)
            .parse()
            .chain_err(|| ErrorKind::InvalidAddress(relay.to_owned()))?;

        let token = Contract::from_json(web3.eth(), token_address, ERC20_ABI.as_bytes())
            .chain_err(|| ErrorKind::InvalidContractAbi)?;
        let relay = Contract::from_json(web3.eth(), relay_address, ERC20_RELAY_ABI.as_bytes())
            .chain_err(|| ErrorKind::InvalidContractAbi)?;

        Ok(Self {
            network_type,
            web3,
            token,
            relay,
        })
    }

    pub fn homechain(transport: T, token: &str, relay: &str) -> Result<Self> {
        Self::new(NetworkType::Home, transport, token, relay)
    }

    pub fn sidechain(transport: T, token: &str, relay: &str) -> Result<Self> {
        Self::new(NetworkType::Side, transport, token, relay)
    }

    pub fn network_type(&self) -> NetworkType {
        self.network_type
    }

    pub fn transfer_stream(
        &self,
        handle: &reactor::Handle,
    ) -> Box<Stream<Item = Transfer, Error = ()>> {
        let (tx, rx) = mpsc::unbounded();
        let filter = FilterBuilder::default()
            .address(vec![self.token.address()])
            .topics(
                Some(vec![TRANSFER_EVENT_SIGNATURE.into()]),
                None,
                Some(vec![self.relay.address().into()]),
                None,
            )
            .build();

        let future = {
            let network_type = self.network_type;
            let tx = tx.clone();
            //let handle = handle.clone();

            self.web3
                .eth_subscribe()
                .subscribe_logs(filter)
                .and_then(move |sub| {
                    sub.for_each(move |log| {
                        if Some(true) == log.removed {
                            warn!("received removed log, revoke votes");
                            return Ok(());
                        }

                        if log.transaction_hash.is_none() {
                            warn!("no transaction hash in transfer");
                            return Ok(());
                        }

                        let tx_hash = log.transaction_hash.unwrap();
                        let destination: Address = log.topics[2].into();
                        let amount: U256 = log.data.0[..32].into();

                        let transfer = Transfer {
                            tx_hash,
                            destination,
                            amount,
                        };

                        trace!("{:?}", &transfer);
                        tx.unbounded_send(transfer).unwrap();

                        Ok(())
                    })
                })
                .map_err(move |e| error!("error in {:?} transfer stream: {:?}", network_type, e))
        };

        handle.spawn(future);

        Box::new(rx)
    }

    pub fn anchor_stream(
        &self,
        handle: &reactor::Handle,
    ) -> Box<Stream<Item = Anchor, Error = ()>> {
        let (tx, rx) = mpsc::unbounded();

        let future = {
            let network_type = self.network_type;
            let tx = tx.clone();

            self.web3
                .eth_subscribe()
                .subscribe_new_heads()
                .and_then(move |sub| {
                    sub.for_each(move |head| {
                        if head.number.is_none() {
                            warn!("no block number in anchor");
                            return Ok(());
                        }

                        if head.hash.is_none() {
                            warn!("no block hash in anchor");
                            return Ok(());
                        }

                        let block_number: U256 = head.number.unwrap().into();
                        let block_hash: H256 = head.hash.unwrap();

                        let anchor = Anchor {
                            block_number,
                            block_hash,
                        };

                        trace!("{:?}", &anchor);
                        tx.unbounded_send(anchor).unwrap();

                        Ok(())
                    })
                })
                .map_err(move |e| error!("error in {:?} anchor stream: {:?}", network_type, e))
        };

        handle.spawn(future);

        Box::new(rx)
    }

    pub fn process_withdrawal(&self, transfer: &Transfer) -> Box<Future<Item = (), Error = Error>> {
        println!("{:?}", transfer);
        Box::new(::web3::futures::future::err("not implemented".into()))
    }

    pub fn anchor(&self, anchor: &Anchor) -> Box<Future<Item = (), Error = Error>> {
        println!("{:?}", anchor);
        Box::new(::web3::futures::future::err("not implemented".into()))
    }
}

impl<T: DuplexTransport> Drop for Network<T> {
    fn drop(&mut self) {
        println!("DROPPING");
    }
}


mod tests {
    use super::*;
    use std::sync::{Arc, atomic};
    use std::collections::BTreeMap;
    use tokio_core;
    use web3::helpers;
    use web3::api::SubscriptionId;
    use web3::futures::{Future, Stream, future};
    use web3::futures::sync::{mpsc};
    use web3::transports::Result;
    use web3::{BatchTransport, DuplexTransport, Error, ErrorKind, RequestId, Transport};
    use parking_lot::Mutex;
    use rpc;

    #[test]
    fn should_build_network_with_mock() {
        Network::new(NetworkType::Home,
            MockTransport::new(),
            "0x5af8bcc6127afde967279dc04661f599a5c0cafa",
            "0x7e7087c25df885f97aeacbfae84ea12016799eee").unwrap();
    }

    #[test]
    fn should_respond_with_add_single_response() {
        let mut eloop = tokio_core::reactor::Core::new().unwrap();
        let mut mock = MockTransport::new();
        mock.clear_rpc();
        let response = rpc::Value::String("asdf".into());
        mock.add_rpc_response(response.clone());

        let finished = eloop.run(mock.execute("eth_accounts", vec![rpc::Value::String("1".into())])).unwrap();

        assert_eq!(finished, response);
    }

        #[test]
    fn should_respond_normally_even_with_extra_data() {
        let mut eloop = tokio_core::reactor::Core::new().unwrap();
        let mut mock = MockTransport::new();
        mock.clear_rpc();
        let response = rpc::Value::String("asdf".into());
        mock.add_batch_rpc_response(vec![response.clone(), response.clone(), response.clone()]);

        let finished = eloop.run(mock.execute("eth_accounts", vec![rpc::Value::String("1".into())])).unwrap();

        assert_eq!(finished, response);
    }

    #[test]
    fn should_respond_with_error_when_no_added_single_response() {
        let mut eloop = tokio_core::reactor::Core::new().unwrap();
        let mut mock = MockTransport::new();
        mock.clear_rpc();
        let response = rpc::Value::String("asdf".into());

        let finished = eloop.run(mock.execute("eth_accounts", vec![rpc::Value::String("1".into())]));

        assert!(finished.is_err());
    }

    #[test]
    fn should_respond_with_result_wrapped_added_batch_response() {
        let mut eloop = tokio_core::reactor::Core::new().unwrap();
        let mut mock = MockTransport::new();
        mock.clear_rpc();
        let response = rpc::Value::String("asdf".into());
        mock.add_batch_rpc_response(vec![response.clone(), response.clone(), response.clone()]);
        let requests = vec![
            mock.prepare("eth_accounts", vec![rpc::Value::String("1".into())]),
            mock.prepare("eth_accounts", vec![rpc::Value::String("1".into())]),
            mock.prepare("eth_accounts", vec![rpc::Value::String("1".into())])
        ];

        let finished = eloop.run(mock.send_batch(requests)).unwrap();

        assert_eq!(finished.len(), 3);
        for value in finished {
            assert_eq!(value.unwrap(), response.clone());
        };
    }

    #[test]
    fn should_respond_with_error_when_no_added_batch_response() {
        let mut eloop = tokio_core::reactor::Core::new().unwrap();
        let mut mock = MockTransport::new();
        mock.clear_rpc();
        let response = rpc::Value::String("asdf".into());

        let requests = vec![
            mock.prepare("eth_accounts", vec![rpc::Value::String("1".into())]),
            mock.prepare("eth_accounts", vec![rpc::Value::String("1".into())]),
            mock.prepare("eth_accounts", vec![rpc::Value::String("1".into())])
        ];

        let finished = eloop.run(mock.send_batch(requests));
        assert!(finished.is_err());
    }

    #[test]
    fn should_have_one_error_when_added_batch_too_short() {
        let mut eloop = tokio_core::reactor::Core::new().unwrap();
        let mut mock = MockTransport::new();
        mock.clear_rpc();
        let response = rpc::Value::String("asdf".into());
        mock.add_batch_rpc_response(vec![response.clone(), response.clone()]);
        let requests = vec![
            mock.prepare("eth_accounts", vec![rpc::Value::String("1".into())]),
            mock.prepare("eth_accounts", vec![rpc::Value::String("1".into())]),
            mock.prepare("eth_accounts", vec![rpc::Value::String("1".into())])
        ];

        let finished = eloop.run(mock.send_batch(requests)).unwrap();
        assert_eq!(finished.len(), 3);
        assert!(finished.get(2).unwrap().is_err());
    }

    pub type MockTask<T> = Box<Future<Item = T, Error = Error>>;

    type Subscription = mpsc::UnboundedSender<rpc::Value>;

    #[derive(Debug, Clone)]
    struct MockTransport {
        id: Arc<atomic::AtomicUsize>,
        responses: Arc<Mutex<Vec<Vec<rpc::Value>>>>,
        subscriptions: Arc<Mutex<BTreeMap<SubscriptionId, Subscription>>>
    }

    impl MockTransport {
        fn new() -> Self {
            let id = Arc::new(atomic::AtomicUsize::new(1));
            let responses: Arc<Mutex<Vec<Vec<rpc::Value>>>> = Default::default();
            let subscriptions: Arc<Mutex<BTreeMap<SubscriptionId, Subscription>>> = Default::default();
            MockTransport {
                id,
                responses,
                subscriptions,
            }
        }

        fn emit_log(&self, log: rpc::Value) {
            for (_id, tx) in self.subscriptions.lock().iter() {
                tx.unbounded_send(log.clone()).unwrap();
            }
        }

        fn emit_head(&self, head: rpc::Value) {
            for (_id, tx) in self.subscriptions.lock().iter() {
                tx.unbounded_send(head.clone()).unwrap();
            }
        }

        fn add_rpc_response(&mut self, response: rpc::Value) {
            self.add_batch_rpc_response(vec![response]);
        }

        fn add_batch_rpc_response(&mut self, responses: Vec<rpc::Value>) {
            self.responses.lock().push(responses);
        }

        fn clear_rpc(&mut self) {
            let mut vec = self.responses.lock();
            *vec = Vec::new();
        }
    }

    impl Transport for MockTransport {
        type Out = MockTask<rpc::Value>;

        fn prepare(&self, method: &str, params: Vec<rpc::Value>) -> (RequestId, rpc::Call) {
            let id = self.id.fetch_add(1, atomic::Ordering::AcqRel);
            let call = helpers::build_request(id, method, params);
            (id, call)
        }

        fn send(&self, _id: RequestId, _request: rpc::Call) -> Self::Out {
            let mut responses = self.responses.lock();
            if responses.len() > 0 {
                let response = responses.remove(0);
                match response.into_iter().next() {
                    Some(value) => {
                        Box::new(future::ok(value))
                    },
                    None => {
                        Box::new(future::err(ErrorKind::Transport("No data available".into()).into()))
                    }
                }
            } else {
                Box::new(future::err(ErrorKind::Transport("No data available".into()).into()))
            }
        }
    }

    impl BatchTransport for MockTransport {
        type Batch = MockTask<Vec<Result<rpc::Value>>>;

        fn send_batch<T>(&self, requests: T) -> Self::Batch
        where
            T: IntoIterator<Item = (RequestId, rpc::Call)>
        {
            let requests: Vec<(usize, rpc::Call)> = requests.into_iter().collect();
            let mut responses = self.responses.lock();
            if responses.len() > 0 {
                let mut batch = Vec::new();
                for value in responses.remove(0) {
                    batch.push(Ok(value));
                }
                for i in batch.len()..requests.len() {
                    batch.push(Err(ErrorKind::Transport("No data available".into()).into()));
                }
                Box::new(future::ok(batch))
            } else {
                Box::new(future::err(ErrorKind::Transport("No data available".into()).into()))
            }
        }
    }

    impl DuplexTransport for MockTransport {
        type NotificationStream = Box<Stream<Item = rpc::Value, Error = Error> + Send + 'static>;

        fn subscribe(&self, id: &SubscriptionId) -> Self::NotificationStream {
            let (tx, rx) = mpsc::unbounded();
            if self.subscriptions.lock().insert(id.clone(), tx).is_some() {
                warn!("Replacing subscription with id {:?}", id);
            }
            Box::new(rx.map_err(|()| ErrorKind::Transport("No data available".into()).into()))
        }

        fn unsubscribe(&self, id: &SubscriptionId) {
            self.subscriptions.lock().remove(id);
        }
    }
}