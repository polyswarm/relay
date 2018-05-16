extern crate clap;
extern crate config;
extern crate ctrlc;
extern crate env_logger;
extern crate tokio_core;
extern crate web3;

#[macro_use]
extern crate error_chain;
#[macro_use]
extern crate log;
#[macro_use]
extern crate serde_derive;
extern crate jsonrpc_core as rpc;
extern crate parking_lot;
extern crate serde_json;

use clap::{App, Arg};

mod contracts;
mod errors;
mod relay;
mod settings;
mod mock;

use errors::*;
use relay::{Network, Relay};
use settings::Settings;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

fn run() -> Result<()> {
    // Set up ctrl-c handler
    let running = Arc::new(AtomicBool::new(true));

    let running_ = running.clone();
    ctrlc::set_handler(move || {
        info!("ctrl-c caught, exiting...");
        running_.store(false, Ordering::SeqCst);
    })?;

    // Set up logger
    env_logger::init();

    // Parse options
    let matches = App::new("Polyswarm Relay")
        .version("0.1.0")
        .author("PolySwarm Developers <info@polyswarm.io>")
        .about("Relays ERC20 tokens between two different networks.")
        .arg(
            Arg::with_name("config")
                .value_name("TOML config file")
                .help("Configures the two networks we will relay between")
                .required(true)
                .takes_value(true),
        )
        .get_matches();
    let settings = Settings::new(matches.value_of("config"))?;

    // Set up our two websocket connections on the same event loop
    let mut eloop = tokio_core::reactor::Core::new()?;
    let handle = eloop.handle();
    let home_ws =
        web3::transports::WebSocket::with_event_loop(&settings.relay.homechain.ws_uri, &handle)?;
    let side_ws =
        web3::transports::WebSocket::with_event_loop(&settings.relay.sidechain.ws_uri, &handle)?;

    let relay = Relay::new(
        Network::homechain(
            home_ws.clone(),
            &settings.relay.homechain.token,
            &settings.relay.homechain.relay,
        )?,
        Network::sidechain(
            side_ws.clone(),
            &settings.relay.sidechain.token,
            &settings.relay.sidechain.relay,
        )?,
    );

    handle.spawn(relay.listen(&handle));

    while running.load(Ordering::SeqCst) {
        eloop.turn(Some(Duration::from_secs(1)));
    }

    Ok(())
}

fn main() {
    if let Err(ref e) = run() {
        error!("error: {}", e);

        for e in e.iter().skip(1) {
            error!("caused by: {}", e);
        }

        if let Some(backtrace) = e.backtrace() {
            error!("backtrace: {:?}", backtrace);
        }

        std::process::exit(1);
    }
}
