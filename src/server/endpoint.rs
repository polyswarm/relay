use serde_derive::{Deserialize, Serialize};
use std::str::FromStr;
use std::thread;
use web3::futures::future;
use web3::futures::prelude::*;
use web3::futures::sync::mpsc;
use web3::types::{Address, H256, U256};

use actix_web::http::StatusCode;
use actix_web::{middleware, web, App, HttpResponse, HttpServer};

use super::errors::EndpointError;
use super::eth::utils;
use super::relay::NetworkType;
use std::collections::HashMap;

pub const HOME: &str = "HOME";
pub const SIDE: &str = "SIDE";

#[derive(Clone)]
pub enum RequestType {
    Hash(NetworkType, H256),
    Status(mpsc::UnboundedSender<Result<StatusResponse, ()>>),
    Balance(NetworkType, mpsc::UnboundedSender<Result<BalanceResponse, ()>>),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct BalanceResponse {
    balances: HashMap<Address, U256>,
}

impl BalanceResponse {
    pub fn new(balances: &HashMap<Address, U256>) -> Self {
        BalanceResponse {
            balances: balances.clone(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct StatusResponse {
    home: NetworkStatus,
    side: NetworkStatus,
}

impl StatusResponse {
    pub fn new(home: NetworkStatus, side: NetworkStatus) -> Self {
        StatusResponse { home, side }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct NetworkStatus {
    relay_eth_balance: Option<U256>,
    relay_last_block: Option<U256>,
    contract_nct_balance: Option<U256>,
}

impl NetworkStatus {
    pub fn new(
        relay_eth_balance: Option<U256>,
        relay_last_block: Option<U256>,
        contract_nct_balance: Option<U256>,
    ) -> Self {
        NetworkStatus {
            relay_eth_balance,
            relay_last_block,
            contract_nct_balance,
        }
    }
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
            HttpServer::new(move || {
                let status_tx = self.tx.clone();
                let hash_tx = self.tx.clone();
                let balance_tx = self.tx.clone();
                App::new()
                    .wrap(middleware::Logger::default())
                    .service(web::resource("/status").route(web::get().to(move || {
                        let tx = status_tx.clone();
                        status(&tx)
                    })))
                    .service(
                        web::resource("/{chain}/balances").route(web::get().to(move |info: web::Path<String>| {
                            let tx = balance_tx.clone();
                            balances(&tx, &info)
                    })))
                    .service(web::resource("/{chain}/{tx_hash}").route(web::post().to(
                        move |info: web::Path<(String, String)>| {
                            let tx = hash_tx.clone();
                            search(&tx, &info)
                    })))
            })
            .bind(format!("0.0.0.0:{}", port))
            .unwrap()
            .run()
            .unwrap();
        });
    }
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
    info: &web::Path<(String, String)>,
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

/// Return an HttpResponse with the list of current balances for all non-contract token holders
///
/// # Arguments
///
/// * `tx` - Sender to report new requests
/// * `info` - Tuple of two strings. The chain and tx hash.
fn balances(
    tx: &mpsc::UnboundedSender<RequestType>,
    info: &str,
) -> Box<Future<Item = HttpResponse, Error = EndpointError>> {
    let chain = if info.to_uppercase() == "HOME" {
        NetworkType::Home
    } else if info.to_uppercase() == "SIDE" {
        NetworkType::Side
    } else {
        return Box::new(future::err(EndpointError::BadChain(info.to_string())));
    };

    let (balance_tx, balance_rx) = mpsc::unbounded();
    let request_future: Box<Future<Item = HttpResponse, Error = EndpointError>> = Box::new(
        balance_rx
            .take(1)
            .collect()
            .and_then(move |messages: Vec<Result<BalanceResponse, ()>>| {
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
                EndpointError::UnableToGetBalances
            }),
    );

    let request = RequestType::Balance(chain, balance_tx);
    let send_result = tx.unbounded_send(request);
    if send_result.is_err() {
        error!("error sending balance request: {:?}", send_result.err());
        return Box::new(future::err(EndpointError::UnableToGetBalances));
    }

    Box::new(request_future)
}
