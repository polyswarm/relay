use rpc;
use serde_json;
use std::cell::RefCell;
use std::collections::vec_deque::VecDeque;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio_core::reactor;
use web3::api::SubscriptionId;
use web3::futures::sync::mpsc;
use web3::futures::{future, Future, Stream};
use web3::helpers;
use web3::transports::Result;
use web3::types::{BlockHeader, Log, H160, H2048, H256, U256};
use web3::{BatchTransport, DuplexTransport, Error, ErrorKind, RequestId, Transport};

use crate::errors::OperationError;
use crate::relay::{Network, NetworkType};

// Result from a MockTask
pub type MockTask<T> = Box<Future<Item = T, Error = Error>>;

// Just hiding the details of the sender
type Subscription = mpsc::UnboundedSender<rpc::Value>;

#[derive(Debug, Clone)]
pub struct MockTransport {
    id: Rc<AtomicUsize>,
    responses: Rc<RefCell<VecDeque<rpc::Value>>>,
    subscriptions: Rc<RefCell<BTreeMap<SubscriptionId, Subscription>>>,
}

impl MockTransport {
    pub fn new() -> Self {
        let id = Rc::new(AtomicUsize::new(1));
        let responses: Rc<RefCell<VecDeque<rpc::Value>>> = Default::default();
        let subscriptions: Rc<RefCell<BTreeMap<SubscriptionId, Subscription>>> = Default::default();
        MockTransport {
            id,
            responses,
            subscriptions,
        }
    }

    pub fn new_network(
        &self,
        network_type: NetworkType,
    ) -> std::result::Result<Network<MockTransport>, OperationError> {
        let tx_count = AtomicUsize::new(0);
        let mock_abi =
            r#"[
            {{
              "constant": true,
              "inputs": [
                {{
                  "name": "_state",
                  "type": "bytes"
                }}
              ],
              "name": "mockConstant",
              "outputs": [
                {{
                  "name": "_flag",
                  "type": "uint8"
                }}
              ],
              "payable": false,
              "stateMutability": "pure",
              "type": "function"
            }}
        ]"#;

        Network::new(
            network_type,
            self.clone(),
            "0x5af8bcc6127afde967279dc04661f599a5c0cafa",
            "0x7e7087c25df885f97aeacbfae84ea12016799eee",
            &mock_abi,
            "0x7e7087c25df885f97aeacbfae84ea12016799eee",
            &mock_abi,
            true,
            0,
            0,
            30,
            30,
            1338,
            "../",
            "password",
            tx_count,
            0,
        )
    }

    pub fn emit_log(&self, log: Log) {
        let value: rpc::Value = serde_json::to_string(&log).unwrap().into();
        for (_id, tx) in self.subscriptions.borrow_mut().iter() {
            tx.unbounded_send(value.clone()).unwrap();
        }
    }

    pub fn emit_head(&self, head: BlockHeader) {
        let value: rpc::Value = serde_json::to_string(&head).unwrap().into();
        for (_id, tx) in self.subscriptions.borrow_mut().iter() {
            tx.unbounded_send(value.clone()).unwrap();
        }
    }

    pub fn add_rpc_response(&mut self, response: rpc::Value) {
        self.responses.borrow_mut().push_back(response)
    }

    pub fn add_batch_rpc_response(&mut self, responses: Vec<rpc::Value>) {
        for response in responses {
            self.add_rpc_response(response);
        }
    }

    pub fn clear_rpc(&mut self) {
        *self.responses.borrow_mut() = VecDeque::new();
    }
}

impl Transport for MockTransport {
    type Out = MockTask<rpc::Value>;

    fn prepare(&self, method: &str, params: Vec<rpc::Value>) -> (RequestId, rpc::Call) {
        let id = self.id.fetch_add(1, Ordering::AcqRel);
        let call = helpers::build_request(id, method, params);
        (id, call)
    }

    fn send(&self, _id: RequestId, _request: rpc::Call) -> Self::Out {
        match self.responses.borrow_mut().pop_front() {
            Some(v) => Box::new(future::finished(v)),
            None => Box::new(future::failed(ErrorKind::Unreachable.into())),
        }
    }
}

impl BatchTransport for MockTransport {
    type Batch = MockTask<Vec<Result<rpc::Value>>>;

    fn send_batch<T>(&self, requests: T) -> Self::Batch
    where
        T: IntoIterator<Item = (RequestId, rpc::Call)>,
    {
        let mut response = Vec::new();
        for _ in requests {
            response.push(match self.responses.borrow_mut().pop_front() {
                Some(v) => Ok(v),
                None => Err(ErrorKind::Unreachable.into()),
            })
        }
        Box::new(future::finished(response))
    }
}

impl DuplexTransport for MockTransport {
    type NotificationStream = Box<Stream<Item = rpc::Value, Error = Error> + Send + 'static>;

    fn subscribe(&self, id: &SubscriptionId) -> Self::NotificationStream {
        let (tx, rx) = mpsc::unbounded();
        if self.subscriptions.borrow_mut().insert(id.clone(), tx).is_some() {
            warn!("replacing subscription with id {:?}", id);
        }
        Box::new(rx.map_err(|()| ErrorKind::Transport("No data available".into()).into()))
    }

    fn unsubscribe(&self, id: &SubscriptionId) {
        self.subscriptions.borrow_mut().remove(id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_build_network_with_mock() {
        let tx_count = AtomicUsize::new(0);

        let mock_abi =
            r#"[
            {{
              "constant": true,
              "inputs": [
                {{
                  "name": "_state",
                  "type": "bytes"
                }}
              ],
              "name": "mockConstant",
              "outputs": [
                {{
                  "name": "_flag",
                  "type": "uint8"
                }}
              ],
              "payable": false,
              "stateMutability": "pure",
              "type": "function"
            }}
        ]"#;

        Network::new(
            NetworkType::Home,
            MockTransport::new(),
            "0x5af8bcc6127afde967279dc04661f599a5c0cafa",
            "0x7e7087c25df885f97aeacbfae84ea12016799eee",
            &mock_abi,
            "0x7e7087c25df885f97aeacbfae84ea12016799eee",
            &mock_abi,
            true,
            0,
            0,
            30,
            30,
            1338,
            "../",
            "password",
            tx_count,
            0,
        )
        .unwrap();
    }

    #[test]
    fn should_respond_with_add_single_response() {
        let mut eloop = reactor::Core::new().unwrap();
        let mut mock = MockTransport::new();
        mock.clear_rpc();
        let response = rpc::Value::String("asdf".into());
        mock.add_rpc_response(response.clone());
        let finished = eloop
            .run(mock.execute("eth_accounts", vec![rpc::Value::String("1".into())]))
            .unwrap();
        assert_eq!(finished, response);
    }

    #[test]
    fn should_respond_normally_even_with_extra_data() {
        let mut eloop = reactor::Core::new().unwrap();
        let mut mock = MockTransport::new();
        mock.clear_rpc();
        let response = rpc::Value::String("asdf".into());
        mock.add_batch_rpc_response(vec![response.clone(), response.clone(), response.clone()]);
        let finished = eloop
            .run(mock.execute("eth_accounts", vec![rpc::Value::String("1".into())]))
            .unwrap();
        assert_eq!(finished, response);
    }

    #[test]
    fn should_respond_with_error_when_no_added_single_response() {
        let mut eloop = reactor::Core::new().unwrap();
        let mut mock = MockTransport::new();
        mock.clear_rpc();
        let finished = eloop.run(mock.execute("eth_accounts", vec![rpc::Value::String("1".into())]));
        assert!(finished.is_err());
    }

    #[test]
    fn should_respond_with_result_wrapped_added_batch_response() {
        let mut eloop = reactor::Core::new().unwrap();
        let mut mock = MockTransport::new();
        mock.clear_rpc();
        let response = rpc::Value::String("asdf".into());
        mock.add_batch_rpc_response(vec![response.clone(), response.clone(), response.clone()]);
        let requests = vec![
            mock.prepare("eth_accounts", vec![rpc::Value::String("1".into())]),
            mock.prepare("eth_accounts", vec![rpc::Value::String("1".into())]),
            mock.prepare("eth_accounts", vec![rpc::Value::String("1".into())]),
        ];
        let finished = eloop.run(mock.send_batch(requests)).unwrap();
        assert_eq!(finished.len(), 3);
        for value in finished {
            assert_eq!(value.unwrap(), response.clone());
        }
    }

    #[test]
    fn should_return_error_per_request_if_no_responses() {
        let mut eloop = reactor::Core::new().unwrap();
        let mut mock = MockTransport::new();
        mock.clear_rpc();
        let requests = vec![
            mock.prepare("eth_accounts", vec![rpc::Value::String("1".into())]),
            mock.prepare("eth_accounts", vec![rpc::Value::String("1".into())]),
            mock.prepare("eth_accounts", vec![rpc::Value::String("1".into())]),
        ];
        let finished = eloop.run(mock.send_batch(requests)).unwrap();
        for request in finished {
            assert!(request.is_err());
        }
    }

    #[test]
    fn should_have_one_error_when_added_batch_too_short() {
        let mut eloop = reactor::Core::new().unwrap();
        let mut mock = MockTransport::new();
        mock.clear_rpc();
        let response = rpc::Value::String("asdf".into());
        mock.add_batch_rpc_response(vec![response.clone(), response.clone()]);
        let requests = vec![
            mock.prepare("eth_accounts", vec![rpc::Value::String("1".into())]),
            mock.prepare("eth_accounts", vec![rpc::Value::String("1".into())]),
            mock.prepare("eth_accounts", vec![rpc::Value::String("1".into())]),
        ];
        let finished = eloop.run(mock.send_batch(requests)).unwrap();
        assert_eq!(finished.len(), 3);
        assert!(finished.get(2).unwrap().is_err());
    }

    #[test]
    fn should_receive_logs_when_emited() {
        let log = Log {
            address: "5af8bcc6127afde967279dc04661f599a5c0cafa".parse().unwrap(),
            topics: Vec::new(),
            data: Default::default(),
            block_hash: None,
            block_number: None,
            transaction_hash: None,
            transaction_index: None,
            log_index: None,
            transaction_log_index: None,
            log_type: None,
            removed: None,
        };
        // Turn Log into an rpc::Value representation
        let value: rpc::Value = serde_json::to_string(&log).unwrap().into();
        // Create event loop & mock
        let mut eloop = reactor::Core::new().unwrap();
        let mock = MockTransport::new();
        // Create future to subscribe and return vec of logs
        let subscription_id = SubscriptionId::from("a".to_owned());
        let stream = mock.subscribe(&subscription_id).collect();
        // Send log to subscribers
        mock.emit_log(log);
        mock.unsubscribe(&subscription_id);
        // Run stream and get back a vector of logs
        let logs = eloop.run(stream).unwrap();
        assert_eq!(value, *logs.get(0).unwrap());
    }

    #[test]
    fn should_receive_header_when_emited() {
        let header = BlockHeader {
            hash: None,
            parent_hash: H256::zero(),
            uncles_hash: H256::zero(),
            author: H160::zero(),
            state_root: H256::zero(),
            transactions_root: H256::zero(),
            receipts_root: H256::zero(),
            number: None,
            gas_used: U256::from(1),
            gas_limit: U256::from(1),
            extra_data: Default::default(),
            logs_bloom: H2048::zero(),
            timestamp: U256::from(1),
            difficulty: U256::from(1),
        };
        // Turn Log into an rpc::Value representation
        let value: rpc::Value = serde_json::to_string(&header).unwrap().into();
        // Create event loop & mock
        let mut eloop = reactor::Core::new().unwrap();
        let mock = MockTransport::new();
        // Create future to subscribe and return vec of logs
        let subscription_id = SubscriptionId::from("a".to_owned());
        let stream = mock.subscribe(&subscription_id).collect();
        // Send log to subscribers
        mock.emit_head(header);
        mock.unsubscribe(&subscription_id);
        // Run stream and get back a vector of logs
        let headers = eloop.run(stream).unwrap();
        assert_eq!(value, *headers.get(0).unwrap());
    }

}
