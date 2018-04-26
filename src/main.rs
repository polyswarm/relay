extern crate clap;
extern crate config;
extern crate ctrlc;
extern crate ethabi;
extern crate web3;

#[macro_use]
extern crate serde_derive;

use clap::{App, Arg};

mod relay;
mod settings;

use relay::{Network, Relay};
use settings::Settings;

fn main() {
    let matches = App::new("Polyswarm Relay")
        .version("0.1.0")
        .author("Polyswarm Developers <info@polyswarm.io>")
        .about("Relays ERC20 tokens between two different networks.")
        .arg(
            Arg::with_name("config")
                .value_name("TOML config file")
                .help("Configures the two networks we will relay between")
                .required(true)
                .takes_value(true),
        )
        .get_matches();

    let config_file = matches
        .value_of("config")
        .expect("You must pass a config file.");
    let settings = Settings::new(config_file).expect("Could not parse config file");

    let wallet = settings.relay.wallet;
    // Let's take the password as either in the config or in an environment variable
    let password = settings.relay.password;

    let homechain_cfg = settings.relay.homechain;
    let sidechain_cfg = settings.relay.sidechain;

    // Stand up the networks in the relay
    let homechain = Network::new(
        "homechain",
        &homechain_cfg.ws_uri,
        &homechain_cfg.token,
        &homechain_cfg.relay,
    );
    let sidechain = Network::new(
        "sidechain",
        &sidechain_cfg.ws_uri,
        &sidechain_cfg.token,
        &sidechain_cfg.relay,
    );

    // Create the relay
    let mut relay = Relay::new(&wallet, &password, homechain, sidechain);

    // Kill the relay & close subscriptions on Ctrl-C
    let b = relay.clone();
    ctrlc::set_handler(move || {
        println!("\rExiting...");
        let mut a = b.clone();
        a.stop();
    }).expect("Unable to setup Ctrl-C handler.");

    // Start relay.
    relay.start();
}
