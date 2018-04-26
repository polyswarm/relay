use std::path::Path;
use config::{Config, ConfigError, File};

#[derive(Deserialize, Debug)]
pub struct Settings {
    pub relay: Relay,
}

#[derive(Deserialize, Debug)]
pub struct Relay {
    pub wallet: String,
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
    pub fn new<P>(path: P) -> Result<Self, ConfigError>
    where
        P: AsRef<Path>,
    {
        let mut s = Config::new();

        let ps = path.as_ref().to_str()
            .ok_or(ConfigError::Message("invalid config path".to_owned()))?;
        s.merge(File::with_name(ps))?;

        s.try_into()
    }
}
