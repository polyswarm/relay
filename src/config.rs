extern crate toml;

use std::io::prelude::*;
use std::fs::File;

#[derive(Deserialize, Debug)]
pub struct Config {
    pub bridge: Bridge,
}

#[derive(Deserialize, Debug)]
pub struct Bridge {
    pub wallet: String,
    pub password: String,
    pub main: Network,
    pub side: Network,
}

#[derive(Deserialize, Debug)]
pub struct Network {
    pub name: String,
    pub host: String,
    pub token: String,
    pub relay: String,
}

pub fn read_config(filename: &str) -> Config {
    let mut config = File::open(filename).expect(&format!("File not found."));
    let mut contents = String::new();

    config
        .read_to_string(&mut contents)
        .expect(&format!("Could not read {}", filename));

    toml::from_str(&contents).expect(&format!("Could not parse config."))
}
