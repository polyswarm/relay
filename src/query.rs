use std::rc::Rc;
use std::thread;
use std::str::FromStr;
use tokio_core::reactor;
use web3::futures::future;
use web3::futures::prelude::*;
use web3::futures::sync::mpsc;
use web3::futures::try_ready;
use web3::types::{Address, TransactionReceipt, H256, U256};
use web3::DuplexTransport;

use actix_web::http::{Method, StatusCode};
use actix_web::{middleware, server, HttpResponse, Path};

use super::contracts::TRANSFER_EVENT_SIGNATURE;
use super::errors::EndpointError;
use super::relay::Network;
use super::transfer::Transfer;
use super::missed_transfer::ValidateAndApproveTransfer;
use super::utils;

pub const HOME: &str = "HOME";
pub const SIDE: &str = "SIDE";

pub struct Endpoint {
    tx: mpsc::UnboundedSender<HashQuery>,
    port: String,
}

impl Endpoint {
    pub fn new(tx: mpsc::UnboundedSender<HashQuery>, port: &str) -> Self {
        Self {
            tx,
            port: port.to_string(),
        }
    }

    pub fn start_server(&self) {
        let tx = self.tx.clone();
        let port = self.port.clone();
        thread::spawn(move || {
            let tx = tx.clone();
            let sys = actix::System::new("relay-endpoint");
            server::new(move || {
                let tx = tx.clone();
                actix_web::App::new()
                    .middleware(middleware::Logger::default())
                    .resource("/{chain}/{tx_hash}", move |r| {
                        r.method(Method::POST).with(move |info: Path<(String, String)>| {
                            let tx = tx.clone();
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

fn search(tx: &mpsc::UnboundedSender<HashQuery>, info: &Path<(String, String)>) -> Result<HttpResponse, EndpointError> {
    let clean = utils::clean_0x(&info.1);
    let tx_hash: H256 = H256::from_str(&clean[..]).map_err(|e| {
        error!("error parsing transaction hash: {:?}", e);
        EndpointError::BadTransactionHash(info.1.clone())
    })?;
    let chain = if info.0.to_uppercase() == "HOME" {
        Ok(QueryChain::Home)
    } else if info.0.to_uppercase() == "SIDE" {
        Ok(QueryChain::Side)
    } else {
        Err(EndpointError::BadChain(info.0.clone()))
    }?;
    let query = HashQuery { chain, tx_hash };
    tx.unbounded_send(query).map_err(|e| {
        error!("error sending query: {:?}", e);
        EndpointError::UnableToSend
    })?;
    Ok(HttpResponse::new(StatusCode::OK))
}

#[derive(Clone)]
pub enum QueryChain {
    Home,
    Side,
}

#[derive(Clone)]
pub struct HashQuery {
    pub chain: QueryChain,
    pub tx_hash: H256,
}

/// Future to handle the Stream of missed transfers by checking them, and approving them
pub struct HandleQueries {
    future: Box<Future<Item = (), Error = ()>>,
}

impl HandleQueries {
    /// Returns a newly created HandleMissedTransfers Future
    ///
    /// # Arguments
    ///
    /// * `source` - Network where the missed transfers are captured
    /// * `target` - Network where the transfer will be approved for a withdrawal
    /// * `handle` - Handle to spawn new futures
    pub fn new<T: DuplexTransport + 'static>(
        homechain: &Rc<Network<T>>,
        sidechain: &Rc<Network<T>>,
        rx: mpsc::UnboundedReceiver<HashQuery>,
        handle: &reactor::Handle,
    ) -> Self {
        let handle = handle.clone();
        let homechain = homechain.clone();
        let sidechain = sidechain.clone();
        let query_future = rx.for_each(move |query| {
            let homechain = homechain.clone();
            let sidechain = sidechain.clone();
            let handle = handle.clone();
            let (source, target) = match query.chain {
                QueryChain::Home => (homechain, sidechain),
                QueryChain::Side => (sidechain, homechain),
            };
            FindTransferInTransaction::new(&source, &query.tx_hash)
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
                .or_else(move |e| {
                    error!("error approving queried transfers: {:?}", e);
                    Ok(())
                })
        });
        HandleQueries {
            future: Box::new(query_future),
        }
    }
}

impl Future for HandleQueries {
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
                        warn!("no relay transactions found at {}", hash);
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
                                                        info!("found log at {}: {:?}", hash, log);
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
                            error!("Could not find requested transaction hash");
                            return Err(());
                        }
                    }?
                }
            };
            self.state = next;
        }
    }
}
