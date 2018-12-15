use base64::decode;
use consul::Client;
use errors::OperationError;
use failure::Error;
use serde_json;
use std::{thread, time};

pub struct ConsulConfig {
    consul_url: String,
    consul_token: String,
    sidechain_name: String,
}

impl ConsulConfig {
    pub fn new(consul_url: &str, consul_token: &str, sidechain_name: &str) -> Self {
        Self {
            consul_url: consul_url.to_string(),
            consul_token: consul_token.to_string(),
            sidechain_name: sidechain_name.to_string(),
        }
    }

    pub fn wait_or_get(&self, chain: &str, key: &str) -> Result<String, Error> {
        let keyname = format!("chain/{}/{}", &self.sidechain_name, &chain);
        let first = Box::new(true);
        let json = self.consul_select(keyname.as_ref(), || {
            if *first {
                info!("chain for config not available in consul yet");
            }
        })?;
        info!("chain for {:?} config available in consul now", chain);

        if json[&key].is_u64() {
            json[&key]
                .as_u64()
                .map_or(Err(OperationError::CouldNotGetConsulKey(key.to_string()).into()), |v| {
                    Ok(v.to_string())
                })
        } else {
            json[&key]
                .as_str()
                .map_or(Err(OperationError::CouldNotGetConsulKey(key.to_string()).into()), |v| {
                    Ok(v.to_string())
                })
        }
    }

    pub fn create_contract_abi(&self, contract_name: &str) -> Result<String, Error> {
        let keyname = format!("chain/{}/{}", &self.sidechain_name, contract_name);
        let json = self.consul_select(keyname.as_ref(), || {
            info!("chain for config not available in consul yet")
        })?;

        Ok(serde_json::ser::to_string(&json["abi"])
            .or_else(|_| Err(OperationError::CouldNotGetConsulKey("abi".into())))?)
    }

    fn consul_select<F>(&self, keyname: &str, mut print_err: F) -> Result<serde_json::Value, Error>
    where
        F: FnMut(),
    {
        let client = Client::new(&self.consul_url, &self.consul_token);
        let keystore = client.keystore;
        let one_sec = time::Duration::from_secs(1);

        loop {
            if let Ok(result) = keystore.get_key(keyname.into()) {
                let result_string = result.unwrap();
                let config = decode(&result_string)?;
                let new_config = String::from_utf8(config)?;
                let json: serde_json::Value = serde_json::from_str(&new_config.as_str())?;
                return Ok(json);
            } else {
                print_err();
                thread::sleep(one_sec);
                continue;
            }
        }
    }
}
