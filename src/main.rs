#[macro_use]
extern crate log;

use ctrlc;
use failure;
use tokio_core;
use web3;

pub mod errors;
pub mod eth;
pub mod events;
pub mod extensions;
#[cfg(test)]
mod mock;
pub mod network;
pub mod relay;
pub mod relay_config;
pub mod server;

use clap::{App, Arg};
use failure::{Error, SyncFailure};
use serde_json;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio_core::reactor;
use web3::futures::sync::mpsc;
use web3::futures::Future;
use web3::Web3;

use crate::endpoint::{Endpoint, RequestType};
use crate::errors::{ConfigError, OperationError};
use crate::network::Network;
use crate::relay::Relay;
use crate::relay_config::{consul_configs, logger, settings};
use crate::server::endpoint;
use crate::settings::Settings;

use log::Level;

fn main() -> Result<(), Error> {
    // Set up ctrl-c handler
    let running = Arc::new(AtomicBool::new(true));

    let running_ = running.clone();
    ctrlc::set_handler(move || {
        info!("ctrl-c caught, exiting...");
        running_.store(false, Ordering::SeqCst);
    })?;

    // Parse options
    let matches = App::new("Polyswarm Relay")
        .version("1.1.0")
        .author("PolySwarm Developers <info@polyswarm.io>")
        .about("Relays ERC20 tokens between two different networks.")
        .arg(
            Arg::with_name("config")
                .short("c")
                .long("config")
                .value_name("TOML config file")
                .help("Configures the two networks we will relay between")
                .required(true)
                .takes_value(true),
        )
        .arg(
            Arg::with_name("log")
                .long("log")
                .value_name("Log level")
                .help("Specifies the logging severity level")
                .takes_value(true),
        )
        .get_matches();

    let settings = Settings::new(matches.value_of("config"))?;

    let log_severity = match matches.value_of("log").unwrap_or("info") {
        "trace" => Level::Trace,
        "debug" => Level::Debug,
        "info" => Level::Info,
        "warn" => Level::Warn,
        "error" => Level::Error,
        _ => Level::Info,
    };

    logger::init_logger(&settings.logging, "relay", log_severity).expect("problem initializing relay logger");

    let consul_config = consul_configs::ConsulConfig::new(
        &settings.relay.consul,
        &settings.relay.consul_token,
        &settings.relay.community,
    );

    let homechain_config = consul_config.wait_or_get("homechain").map_err(|e| {
        error!("error fetchign consul configuration {:?}", e);
        ConfigError::ConsulError
    })?;
    let sidechain_config = consul_config.wait_or_get("sidechain").map_err(|e| {
        error!("error fetchign consul configuration {:?}", e);
        ConfigError::ConsulError
    })?;

    let nectar_token_abi = consul_config.create_contract_abi("NectarToken")?;
    let erc20_relay_abi = consul_config.create_contract_abi("ERC20Relay")?;

    consul_config.watch_for_config_deletion();

    // Set up our two websocket connections on the same event loop
    let mut eloop = tokio_core::reactor::Core::new()?;
    let handle = eloop.handle();

    let home_ws = web3::transports::WebSocket::with_event_loop(&settings.relay.homechain.wsuri, &handle)
        .map_err(SyncFailure::new)?;

    let side_ws = web3::transports::WebSocket::with_event_loop(&settings.relay.sidechain.wsuri, &handle)
        .map_err(SyncFailure::new)?;

    let (tx, rx) = mpsc::unbounded();
    let endpoint = Endpoint::new(tx, settings.endpoint.port);
    endpoint.start_server();
    // Run the relay
    handle.spawn(run(
        handle.clone(),
        rx,
        settings,
        home_ws,
        homechain_config,
        side_ws,
        sidechain_config,
        nectar_token_abi,
        erc20_relay_abi,
    ));

    while running.load(Ordering::SeqCst) {
        eloop.turn(Some(Duration::from_secs(1)));
    }

    Ok(())
}

fn run(
    handle: reactor::Handle,
    hash_rx: mpsc::UnboundedReceiver<RequestType>,
    settings: Settings,
    home_ws: web3::transports::WebSocket,
    home_config: serde_json::Value,
    side_ws: web3::transports::WebSocket,
    side_config: serde_json::Value,
    nectar_token_abi: String,
    erc20_relay_abi: String,
) -> impl Future<Item = (), Error = ()> {
    let account = eth::utils::clean_0x(&settings.relay.account)
        .parse()
        .or_else(|_| Err(OperationError::InvalidAddress(settings.relay.account.clone())))
        .unwrap();

    let home_web3 = Web3::new(home_ws.clone());
    let side_web3 = Web3::new(side_ws.clone());

    home_web3
        .eth()
        .transaction_count(account, None)
        .and_then(move |home_nonce| {
            side_web3
                .eth()
                .transaction_count(account, None)
                .and_then(move |side_nonce| {
                    let mut _home_nonce = AtomicUsize::new(home_nonce.as_u64() as usize);
                    let mut _side_nonce = AtomicUsize::new(side_nonce.as_u64() as usize);

                    let mut key = "chain_id";
                    let home_chain_id = home_config[key]
                        .as_u64()
                        .ok_or_else(|| OperationError::CouldNotGetConsulKey(key.to_string()))
                        .map_err(|e| e.to_string())?;
                    let side_chain_id = side_config[key]
                        .as_u64()
                        .ok_or_else(|| OperationError::CouldNotGetConsulKey(key.to_string()))
                        .map_err(|e| e.to_string())?;

                    key = "nectar_token_address";
                    let homechain_nectar_token_address = home_config[key]
                        .as_str()
                        .ok_or_else(|| OperationError::CouldNotGetConsulKey(key.to_string()))
                        .map_err(|e| e.to_string())?;
                    let sidechain_nectar_token_address = side_config[key]
                        .as_str()
                        .ok_or_else(|| OperationError::CouldNotGetConsulKey(key.to_string()))
                        .map_err(|e| e.to_string())?;

                    key = "erc20_relay_address";
                    let homechain_erc20_relay_address = home_config[key]
                        .as_str()
                        .ok_or_else(|| OperationError::CouldNotGetConsulKey(key.to_string()))
                        .map_err(|e| e.to_string())?;
                    let sidechain_erc20_relay_address = side_config[key]
                        .as_str()
                        .ok_or_else(|| OperationError::CouldNotGetConsulKey(key.to_string()))
                        .map_err(|e| e.to_string())?;

                    let relay = Relay::new(
                        Network::homechain(
                            home_ws.clone(),
                            &settings.relay.account,
                            &homechain_nectar_token_address,
                            &nectar_token_abi,
                            &homechain_erc20_relay_address,
                            &erc20_relay_abi,
                            settings.relay.homechain.free,
                            settings.relay.confirmations,
                            settings.relay.homechain.interval,
                            settings.relay.homechain.timeout,
                            home_chain_id,
                            &settings.relay.keydir,
                            &settings.relay.password,
                            _home_nonce,
                            settings.relay.retries,
                        )
                        .map_err(|e| format!("error initializing homechain {}", e))?,
                        Network::sidechain(
                            side_ws.clone(),
                            &settings.relay.account,
                            &sidechain_nectar_token_address,
                            &nectar_token_abi,
                            &sidechain_erc20_relay_address,
                            &erc20_relay_abi,
                            settings.relay.sidechain.free,
                            settings.relay.confirmations,
                            settings.relay.anchor_frequency,
                            settings.relay.sidechain.interval,
                            settings.relay.sidechain.timeout,
                            side_chain_id,
                            &settings.relay.keydir,
                            &settings.relay.password,
                            _side_nonce,
                            settings.relay.retries,
                        )
                        .map_err(|e| format!("error initializing sidechain {}", e))?,
                    );
                    handle.spawn(relay.run(hash_rx, &handle));
                    Ok(())
                })
        })
        .map_err(|e| {
            error!("error getting transaction count: {:?}", e);
        })
}
