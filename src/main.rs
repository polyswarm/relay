extern crate base64;
extern crate clap;
extern crate config;
extern crate consul;
extern crate ctrlc;
extern crate env_logger;
extern crate tokio_core;
extern crate web3;

extern crate failure;
#[macro_use]
extern crate failure_derive;
#[macro_use]
extern crate log;
extern crate jsonrpc_core as rpc;
extern crate parking_lot;
#[macro_use]
extern crate serde_derive;
#[macro_use]
extern crate serde_json;

use clap::{App, Arg};

pub mod consul_configs;
pub mod contracts;
pub mod errors;
pub mod logger;
pub mod relay;
pub mod settings;

#[cfg(test)]
mod mock;

use failure::{Error, SyncFailure};
use relay::{Network, Relay};
use settings::Settings;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

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
        .version("0.1.0")
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
        ).get_matches();

    let settings = Settings::new(matches.value_of("config"))?;

    logger::init_logger(settings.logging, "relay", Level::Debug).expect("problem initializing relay logger");

    // Set up our two websocket connections on the same event loop
    let mut eloop = tokio_core::reactor::Core::new()?;
    let handle = eloop.handle();
    let home_ws = web3::transports::WebSocket::with_event_loop(&settings.relay.homechain.wsuri, &handle)
        .map_err(SyncFailure::new)?;
    let side_ws = web3::transports::WebSocket::with_event_loop(&settings.relay.sidechain.wsuri, &handle)
        .map_err(SyncFailure::new)?;

    let relay = Relay::new(
        Network::homechain(
            home_ws.clone(),
            &settings.relay.account,
            &*consul_configs::wait_or_get("homechain", "nectar_token_address"),
            &*consul_configs::wait_or_get("homechain", "erc20_relay_address"),
            &settings.relay.homechain.free,
            settings.relay.confirmations,
        )?,
        Network::sidechain(
            side_ws.clone(),
            &settings.relay.account,
            &*consul_configs::wait_or_get("sidechain", "nectar_token_address"),
            &*consul_configs::wait_or_get("sidechain", "erc20_relay_address"),
            &settings.relay.sidechain.free,
            settings.relay.confirmations,
            settings.relay.anchor_frequency,
        )?,
    );

    // Unlock accounts now
    eloop.run(relay.unlock(&settings.relay.password))?;

    // Run the relay
    handle.spawn(relay.run(&handle));
    while running.load(Ordering::SeqCst) {
        eloop.turn(Some(Duration::from_secs(1)));
    }

    Ok(())
}
