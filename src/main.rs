#[macro_use]
extern crate serde_derive;
extern crate toml;
extern crate clap;
extern crate web3;
extern crate ethabi;

use clap::{App, Arg};

mod relay;
mod config;

fn main() {
    let matches = App::new("Polyswarm Relay Bridge.")
                    .version("0.0.1")
                    .author("Robert Lathrop <rl@polyswarm.io>")
                    .about("Bridges between two contracts on different networks.")
                    .arg(Arg::with_name("config")
                        .value_name("TOML configuration file")
                        .help("Configures the two networks we will bridge")
                        .required(true)
                        .takes_value(true))
                    .get_matches();
    
    let config_file = matches.value_of("config")
        .expect("You must pass a config file.");

    let configuration = config::read_config(config_file);

    println!("Configuration {:?}", configuration);

    let main = configuration.bridge.main;

    let private = relay::Network::new(&main.name, &main.host, &main.token, &main.relay);
    private.listen();
}
