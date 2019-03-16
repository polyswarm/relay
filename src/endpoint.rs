use ethabi::Token;
use serde_derive::{Deserialize, Serialize};
use std::rc::Rc;
use std::str::FromStr;
use std::thread;
use tokio_core::reactor;
use web3::contract;
use web3::contract::tokens::{Detokenize, Tokenize};
use web3::contract::Options;
use web3::futures::future;
use web3::futures::prelude::*;
use web3::futures::sync::mpsc;
use web3::futures::try_ready;
use web3::types::{Address, BlockNumber, TransactionReceipt, H256, U256};
use web3::DuplexTransport;

use actix_web::http::{Method, StatusCode};
use actix_web::{middleware, server, HttpResponse, Path};

use super::contracts::TRANSFER_EVENT_SIGNATURE;
use super::errors::EndpointError;
use super::missed_transfer::ValidateAndApproveTransfer;
use super::relay::{Network, NetworkType};
use super::transfer::Transfer;
use super::utils;

pub const HOME: &str = "HOME";
pub const SIDE: &str = "SIDE";

#[derive(Clone)]
pub enum RequestType {
    Hash(NetworkType, H256),
    Status(mpsc::UnboundedSender<Result<StatusResponse, ()>>),
}

/// This defines the http endpoint used to request a look at a specific transaction hash
#[derive(Clone)]
pub struct Endpoint {
    tx: mpsc::UnboundedSender<RequestType>,
    port: String,
}

impl Endpoint {
    /// Returns a newly created Endpoint Struct
    ///
    /// # Arguments
    ///
    /// * `tx` - Sender to report new queries
    /// * `port` - Handle to spawn new futures
    pub fn new(tx: mpsc::UnboundedSender<RequestType>, port: u16) -> Self {
        Self {
            tx,
            port: port.to_string(),
        }
    }

    /// Start listening on the given port for messages at /chain/tx_hash
    pub fn start_server(self) {
        let port = self.port.clone();
        thread::spawn(move || {
            let sys = actix::System::new("relay-endpoint");
            server::new(move || {
                let status_tx = self.tx.clone();
                let hash_tx = self.tx.clone();
                actix_web::App::new()
                    .middleware(middleware::Logger::default())
                    .resource("/status", move |r| {
                        r.method(Method::GET).f(move |_| {
                            let tx = status_tx.clone();
                            status(&tx)
                        })
                    })
                    .resource("/{chain}/{tx_hash}", move |r| {
                        r.method(Method::POST).with(move |info: Path<(String, String)>| {
                            let tx = hash_tx.clone();
                            search(&tx, &info)
                        })
                    })
                    .finish()
            })
            .bind(format!("0.0.0.0:{}", port))
            .unwrap()
            .start();
            let _ = sys.run();
        });
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct StatusResponse {
    home_eth: Option<U256>,
    home_nct: Option<U256>,
    home_last_block: Option<U256>,
    side_nct: Option<U256>,
    side_last_block: Option<U256>,
}

/// Return an HttpResponse that contains the status of this relay
///
/// # Arguments
///
/// * `tx` - Sender to report new requests
fn status(tx: &mpsc::UnboundedSender<RequestType>) -> Box<Future<Item = HttpResponse, Error = EndpointError>> {
    let (status_tx, status_rx) = mpsc::unbounded();
    let request_future: Box<Future<Item = HttpResponse, Error = EndpointError>> = Box::new(
        status_rx
            .take(1)
            .collect()
            .and_then(move |messages: Vec<Result<StatusResponse, ()>>| {
                if !messages.is_empty() {
                    Ok(messages[0].clone())
                } else {
                    Err(())
                }
            })
            .and_then(move |msg| match msg {
                Ok(response) => {
                    let body = serde_json::to_string(&response).map_err(move |e| {
                        error!("error parsing response: {:?}", e);
                    })?;

                    Ok(HttpResponse::Ok().content_type("application/json").body(body))
                }
                Err(_) => Err(()),
            })
            .map_err(|_| {
                error!("error receiving message");
                EndpointError::UnableToGetStatus
            }),
    );

    let request = RequestType::Status(status_tx);
    let send_result = tx.unbounded_send(request);
    if send_result.is_err() {
        error!("error sending status request: {:?}", send_result.err());
        return Box::new(future::err(EndpointError::UnableToGetStatus));
    }

    Box::new(request_future)
}

/// Return an HttpResponse given the success of sending the txhash and chain to be scanned
///
/// # Arguments
///
/// * `tx` - Sender to report new requests
/// * `info` - Tuple of two strings. The chain and tx hash.
fn search(
    tx: &mpsc::UnboundedSender<RequestType>,
    info: &Path<(String, String)>,
) -> Result<HttpResponse, EndpointError> {
    let clean = utils::clean_0x(&info.1);
    let tx_hash: H256 = H256::from_str(&clean[..]).map_err(|e| {
        error!("error parsing transaction hash: {:?}", e);
        EndpointError::BadTransactionHash(info.1.clone())
    })?;
    let chain = if info.0.to_uppercase() == "HOME" {
        Ok(NetworkType::Home)
    } else if info.0.to_uppercase() == "SIDE" {
        Ok(NetworkType::Side)
    } else {
        Err(EndpointError::BadChain(info.0.clone()))
    }?;
    let request = RequestType::Hash(chain, tx_hash);
    tx.unbounded_send(request).map_err(|e| {
        error!("error sending hash request: {:?}", e);
        EndpointError::UnableToSend
    })?;
    Ok(HttpResponse::new(StatusCode::OK))
}

/// Future to handle the Stream of missed transfers by checking them, and approving them
pub struct HandleRequests {
    future: Box<Future<Item = (), Error = ()>>,
}

impl HandleRequests {
    /// Returns a newly created HandleMissedTransfers Future
    ///
    /// # Arguments
    ///
    /// * `source` - Network where the missed transfers are captured
    /// * `target` - Network where the transfer will be approved for a withdrawal
    /// * `rx` - Receiver where requested RequestTypes will come across
    /// * `handle` - Handle to spawn new futures
    pub fn new<T: DuplexTransport + 'static>(
        homechain: &Rc<Network<T>>,
        sidechain: &Rc<Network<T>>,
        rx: mpsc::UnboundedReceiver<RequestType>,
        handle: &reactor::Handle,
    ) -> Self {
        let handle = handle.clone();
        let homechain = homechain.clone();
        let sidechain = sidechain.clone();
        let requests_future = rx.for_each(move |request| {
            let homechain = homechain.clone();
            let sidechain = sidechain.clone();
            let handle = handle.clone();
            match request {
                RequestType::Hash(chain, tx_hash) => {
                    let (source, target) = match chain {
                        NetworkType::Home => (homechain, sidechain),
                        NetworkType::Side => (sidechain, homechain),
                    };
                    future::Either::A(
                        FindTransferInTransaction::new(&source, &tx_hash)
                            .and_then(move |transfers| {
                                let handle = handle.clone();
                                let target = target.clone();
                                let futures: Vec<ValidateAndApproveTransfer<T>> = transfers
                                    .iter()
                                    .map(move |transfer| {
                                        let handle = handle.clone();
                                        let target = target.clone();
                                        ValidateAndApproveTransfer::new(&target, &handle, &transfer)
                                    })
                                    .collect();
                                future::join_all(futures)
                            })
                            .and_then(|_| Ok(()))
                            .or_else(move |_| {
                                // No log here, errors are caught in Futures
                                Ok(())
                            }),
                    )
                }
                RequestType::Status(tx) => {
                    let home_eth_future = homechain
                        .web3
                        .eth()
                        .balance(homechain.account, None)
                        .and_then(move |balance| Ok(Some(balance)))
                        .or_else(|_| Ok(None));

                    let home_balance_query = BalanceQuery::new(homechain.account);
                    let home_nct_future = homechain
                        .token
                        .query::<BalanceOf, Address, BlockNumber, BalanceQuery>(
                            "balanceOf",
                            home_balance_query,
                            homechain.account,
                            Options::default(),
                            BlockNumber::Latest,
                        )
                        .and_then(move |balance| Ok(Some(balance.0)))
                        .or_else(move |_| Ok(None));

                    let home_last_block_future = homechain
                        .web3
                        .eth()
                        .block_number()
                        .and_then(move |block| Ok(Some(block)))
                        .or_else(|_| Ok(None));

                    let side_balance_query = BalanceQuery::new(sidechain.account);
                    let side_nct_future = sidechain
                        .token
                        .query::<BalanceOf, Address, BlockNumber, BalanceQuery>(
                            "balanceOf",
                            side_balance_query,
                            sidechain.account,
                            Options::default(),
                            BlockNumber::Latest,
                        )
                        .and_then(move |balance| Ok(Some(balance.0)))
                        .or_else(move |_| Ok(None));

                    let side_last_block_future = sidechain
                        .web3
                        .eth()
                        .block_number()
                        .and_then(move |block| Ok(Some(block)))
                        .or_else(|_| Ok(None));

                    let futures: Vec<Box<Future<Item = Option<U256>, Error = ()>>> = vec![
                        Box::new(home_eth_future),
                        Box::new(home_nct_future),
                        Box::new(home_last_block_future),
                        Box::new(side_nct_future),
                        Box::new(side_last_block_future),
                    ];

                    let tx = tx.clone();
                    let error_tx = tx.clone();

                    future::Either::B(
                        future::join_all(futures)
                            .and_then(move |results| {
                                info!("results from status futures: {:?}", results);
                                Ok(StatusResponse {
                                    home_eth: results[0],
                                    home_nct: results[1],
                                    home_last_block: results[2],
                                    side_nct: results[3],
                                    side_last_block: results[4],
                                })
                            })
                            .and_then(move |response| {
                                info!("Status response: {:?}", response);
                                let tx: mpsc::UnboundedSender<Result<StatusResponse, ()>> = tx.clone();
                                let send_result = tx.unbounded_send(Ok(response));
                                if send_result.is_err() {
                                    error!("error sending status response");
                                }
                                info!("sent response");
                                Ok(())
                            })
                            .or_else(move |e| {
                                let tx = error_tx.clone();
                                error!("error getting status: {:?}", e);
                                let send_result = tx.unbounded_send(Err(()));
                                if send_result.is_err() {
                                    error!("error sending status response");
                                }
                                Ok(())
                            }),
                    )
                }
            }
        });

        HandleRequests {
            future: Box::new(requests_future),
        }
    }
}

impl Future for HandleRequests {
    type Item = ();
    type Error = ();
    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        self.future.poll()
    }
}

pub enum FindTransferState {
    ExtractTransfers(Box<Future<Item = Vec<Transfer>, Error = ()>>),
    FetchReceipt(Box<Future<Item = Option<TransactionReceipt>, Error = ()>>),
}

/// Future to find a vec of transfers at a specific transaction
pub struct FindTransferInTransaction<T: DuplexTransport + 'static> {
    hash: H256,
    source: Rc<Network<T>>,
    state: FindTransferState,
}

impl<T: DuplexTransport + 'static> FindTransferInTransaction<T> {
    /// Returns a newly created FindTransferInTransaction Future
    ///
    /// # Arguments
    ///
    /// * `source` - Network where the transaction took place
    /// * `hash` - Transaction hash to check
    fn new(source: &Rc<Network<T>>, hash: &H256) -> Self {
        let web3 = source.web3.clone();
        let future = web3.clone().eth().transaction_receipt(*hash).map_err(|e| {
            error!("error getting transaction receipt: {:?}", e);
        });
        let state = FindTransferState::FetchReceipt(Box::new(future));
        FindTransferInTransaction {
            hash: *hash,
            source: source.clone(),
            state,
        }
    }
}

impl<T: DuplexTransport + 'static> Future for FindTransferInTransaction<T> {
    type Item = Vec<Transfer>;
    type Error = ();
    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        loop {
            let source = self.source.clone();
            let hash = self.hash;
            let next = match self.state {
                FindTransferState::ExtractTransfers(ref mut future) => {
                    let transfers = try_ready!(future.poll());
                    if transfers.is_empty() {
                        warn!("no relay transactions found at {:?}", hash);
                        return Err(());
                    } else {
                        return Ok(Async::Ready(transfers));
                    }
                }
                FindTransferState::FetchReceipt(ref mut future) => {
                    let receipt = try_ready!(future.poll());
                    match receipt {
                        Some(r) => {
                            if r.block_hash.is_none() {
                                error!("receipt did not have block hash");
                                return Err(());
                            }
                            let block_hash = r.block_hash.unwrap();
                            r.block_number.map_or_else(
                                || {
                                    error!("receipt did not have block number");
                                    Err(())
                                },
                                |receipt_block| {
                                    let future = source
                                        .web3
                                        .clone()
                                        .eth()
                                        .block_number()
                                        .and_then(move |block| {
                                            let mut transfers = Vec::new();
                                            if let Some(confirmed) =
                                                block.checked_rem(receipt_block).map(|u| u.as_u64())
                                            {
                                                if confirmed > source.confirmations {
                                                    let logs = r.logs;
                                                    for log in logs {
                                                        info!("found log at {:?}: {:?}", hash, log);
                                                        if log.topics[0] == TRANSFER_EVENT_SIGNATURE.into()
                                                            && log.topics[2] == source.relay.address().into()
                                                        {
                                                            let destination: Address = log.topics[1].into();
                                                            let amount: U256 = log.data.0[..32].into();
                                                            if destination == Address::zero() {
                                                                info!("found mint. Skipping");
                                                                continue;
                                                            }
                                                            let transfer = Transfer {
                                                                destination,
                                                                amount,
                                                                tx_hash: hash,
                                                                block_hash,
                                                                block_number: receipt_block,
                                                            };
                                                            transfers.push(transfer);
                                                        }
                                                    }
                                                }
                                            }
                                            Ok(transfers)
                                        })
                                        .map_err(|e| {
                                            error!("error getting current block: {:?}", e);
                                        });
                                    Ok(FindTransferState::ExtractTransfers(Box::new(future)))
                                },
                            )
                        }
                        None => {
                            error!("unable to find {:?} on {:?}", hash, source.network_type);
                            return Err(());
                        }
                    }?
                }
            };
            self.state = next;
        }
    }
}

pub struct BalanceQuery {
    address: Address,
}

impl BalanceQuery {
    fn new(address: Address) -> Self {
        BalanceQuery { address }
    }
}

impl Tokenize for BalanceQuery {
    fn into_tokens(self) -> Vec<Token> {
        vec![Token::Address(self.address)]
    }
}

/// Withdrawal event added to contract after a transfer
#[derive(Debug, Clone)]
pub struct BalanceOf(U256);

impl Detokenize for BalanceOf {
    /// Creates a new instance from parsed ABI tokens.
    fn from_tokens(tokens: Vec<Token>) -> Result<Self, contract::Error>
    where
        Self: Sized,
    {
        let balance = tokens[0].clone().to_uint().ok_or_else(|| {
            contract::Error::from_kind(contract::ErrorKind::Msg(
                "cannot parse balance from contract response".to_string(),
            ))
        })?;
        info!("balance of: {:?}", balance);
        Ok(BalanceOf(balance))
    }
}
