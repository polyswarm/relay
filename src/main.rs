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
                    .arg(Arg::with_name("address")
                        .short("a")
                        .long("address")
                        .value_name("Token Contract Address")
                        .help("Sets address to filter")
                        .takes_value(true))
                    .arg(Arg::with_name("to_address")
                        .long("to-address")
                        .value_name("Address to capture")
                        .help("Sets Transfer to address to filter against")
                        .takes_value(true))
                    .arg(Arg::with_name("port")
                        .short("p")
                        .long("port")
                        .value_name("Number")
                        .help("Sets port we are listening on")
                        .takes_value(true))
                    .get_matches();
    
    let address = matches.value_of("address").unwrap_or("0x9e46a38f5daabe8683e10793b06749eef7d733d1");
    println!("address: {}", address);

    let to_address = matches.value_of("to_address").unwrap_or("0x0000000000000000000000000000000000000000");
    println!("to_address: {}", to_address);

    let port = matches.value_of("port").unwrap_or("8545");
    println!("port: {}", port);

    let host = "ws://localhost";

    let home = relay::Relay::new(host, port, address, to_address);
    home.listen();
}
