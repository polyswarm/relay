extern crate clap;
extern crate web3;
extern crate ethabi;

use clap::{App, Arg};

mod relay;

fn main() {
    let matches = App::new("Polyswarm Relay Bridge.")
                    .version("0.0.1")
                    .author("Robert Lathrop <rl@polyswarm.io>")
                    .about("Bridges between two contracts on different networks.")
                    .arg(Arg::with_name("token")
                        .short("t")
                        .long("token-address")
                        .value_name("Token Contract Address")
                        .help("Sets address to filter")
                        .takes_value(true))
                    .arg(Arg::with_name("relay")
                        .short("r")
                        .long("relay-address")
                        .value_name("Relay address to monitor deposits")
                        .help("Sets Transfer to address to filter against")
                        .takes_value(true))
                    .arg(Arg::with_name("host")
                        .short("h")
                        .long("host")
                        .value_name("URI")
                        .help("Sets geth Websocket host we are listening on")
                        .takes_value(true))
                    .get_matches();
    
    let token = matches.value_of("token").unwrap_or("0x9e46a38f5daabe8683e10793b06749eef7d733d1");
    println!("token: {}", token);

    let relay = matches.value_of("relay").unwrap_or("0x0000000000000000000000000000000000000000");
    println!("relay: {}", relay);

    let host = matches.value_of("host").unwrap_or("ws://localhost:8545");
    println!("host: {}", host);

    let private = relay::Network::new("PrivateTestnet", host, token, relay);
    private.listen();
}
