use config::{Config, Environment, File};
use failure::Error;
use std::env;
use std::ffi::OsString;
use std::path::Path;

use super::errors::ConfigError;
use super::missed_transfer::{LOOKBACK_LEEWAY, LOOKBACK_RANGE};

/// Settings for the application
#[derive(Debug, Deserialize)]
pub struct Settings {
    /// Relay settings
    pub relay: Relay,
    pub logging: Logging,
}

/// Logging settings
///
/// Currently just formatting style, but future loggers can complex embed options in their variant,
/// so we derive Clone
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "format", rename_all = "kebab-case")]
pub enum Logging {
    Raw,
    Json,
}

/// Relay settings
#[derive(Debug, Deserialize)]
pub struct Relay {
    /// The account to send transactions from
    pub account: String,
    /// The password to unlock the account
    pub password: String,
    /// Number of blocks to wait for confirmation
    pub confirmations: u64,
    /// Frequency of sidechain anchor blocks
    pub anchor_frequency: u64,
    /// Network to use as the homechain
    pub homechain: Network,
    /// Network to use as the sidechain
    pub sidechain: Network,
    /// consul url to grab contracts from
    pub consul: String,
    /// consul token used to access consul
    pub consul_token: String,
    /// community name for consul kv
    pub community: String,
    /// keyfile used to confirm blocks
    pub keydir: String,
}

/// Per-network settings
#[derive(Debug, Deserialize)]
pub struct Network {
    /// URI for the Websocket RPC endpoint for an Ethereum client
    pub wsuri: String,
    /// Whether or not the transactions should be free
    pub free: bool,
    /// seconds between checks for missed transactions
    pub interval: u64,
    /// Chain id for an Ethereum client
    pub chain_id: u64,
}

impl Settings {
    /// Construct a new settings object from a file path and the environment
    ///
    /// # Arguments
    ///
    /// * `path` - Path to a configuration file
    pub fn new<P>(path: Option<P>) -> Result<Self, Error>
    where
        P: AsRef<Path>,
    {
        let mut c = Config::new();

        c.set_default("relay.confirmations", 12)?;
        c.set_default("relay.anchor_frequency", 100)?;
        c.set_default("relay.community", "")?;
        c.set_default("relay.homechain.interval", 30)?;
        c.set_default("relay.sidechain.interval", 30)?;

        // XXX: Get default from the CONSUL_TOKEN environment variable, look into naming such that
        // below Environment override does this for us
        c.set_default(
            "relay.consul_token",
            env::var_os("CONSUL_TOKEN")
                .unwrap_or_else(|| OsString::from(""))
                .to_string_lossy()
                .to_string(),
        )?;

        if let Some(p) = path {
            let ps = p.as_ref().to_str().ok_or(ConfigError::InvalidConfigFilePath)?;
            c.merge(File::with_name(ps))?;
        }

        c.merge(Environment::new())?;

        c.try_into()
            .map_err(|e| e.into())
            .and_then(|s: Self| s.validated().map_err(|e| e.into()))
    }

    fn validated(self) -> Result<Self, ConfigError> {
<<<<<<< HEAD
        let lookback_combined = LOOKBACK_RANGE + LOOKBACK_LEEWAY;
=======
>>>>>>> error msg for side and home chain nonce
        if self.relay.anchor_frequency == 0 {
            Err(ConfigError::InvalidAnchorFrequency)
        } else if self.relay.confirmations >= self.relay.anchor_frequency {
            Err(ConfigError::InvalidConfirmations)
        } else if self.relay.homechain.interval >= lookback_combined
            || self.relay.sidechain.interval >= lookback_combined
        {
            Err(ConfigError::InvalidLookbackInterval(lookback_combined))
        } else if !Path::new(&self.relay.keydir).exists() {
            Err(ConfigError::InvalidKeydir)
        } else {
            Ok(self)
        }
    }
}
