use std::path::Path;
use config::{Config, Environment, File};

use super::errors::*;

#[derive(Deserialize, Debug)]
pub struct Settings {
    pub relay: Relay,
}

#[derive(Deserialize, Debug)]
pub struct Relay {
    pub account: String,
    pub password: String,
    pub homechain: Network,
    pub sidechain: Network,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "kebab-case")]
pub struct Network {
    pub ws_uri: String,
    pub token: String,
    pub relay: String,
}

impl Settings {
    pub fn new<P>(path: Option<P>) -> Result<Self>
    where
        P: AsRef<Path>,
    {
        let mut s = Config::new();

        if let Some(p) = path {
            let ps = p.as_ref()
                .to_str()
                .chain_err(|| "invalid config file path")?;
            s.merge(File::with_name(ps))?;
        }

        s.merge(Environment::new())?;

        s.try_into().map_err(|e| e.into())
    }
}
