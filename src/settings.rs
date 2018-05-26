use config::{Config, Environment, File};
use std::path::Path;

use super::errors::*;

#[derive(Debug, Deserialize)]
pub struct Settings {
    pub relay: Relay,
}

#[derive(Debug, Deserialize)]
pub struct Relay {
    pub account: String,
    pub password: String,
    pub confirmations: u64,
    pub anchor_frequency: u64,
    pub homechain: Network,
    pub sidechain: Network,
}

#[derive(Debug, Deserialize)]
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
        let mut c = Config::new();

        c.set_default("relay.confirmations", 12)?;
        c.set_default("relay.anchor_frequency", 100)?;

        if let Some(p) = path {
            let ps = p.as_ref().to_str().chain_err(|| ErrorKind::InvalidConfigFilePath)?;
            c.merge(File::with_name(ps))?;
        }

        c.merge(Environment::new())?;
        c.try_into().map_err(|e| e.into()).and_then(|s: Self| s.validated())
    }

    fn validated(self) -> Result<Self> {
        if self.relay.anchor_frequency == 0 {
            Err(ErrorKind::InvalidAnchorFrequency.into())
        } else if self.relay.confirmations >= self.relay.anchor_frequency {
            Err(ErrorKind::InvalidConfirmations.into())
        } else {
            Ok(self)
        }
    }
}
