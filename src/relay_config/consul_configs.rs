use crate::errors::OperationError;
use base64::decode;
use consul::{kv::KV, Client, Config};
use failure::Error;
use serde_json;
use std::collections::HashMap;
use std::{process, thread, time};

#[derive(Debug, Clone)]
pub struct ConsulConfig {
    consul_url: String,
    consul_token: String,
    community: String,
}

impl ConsulConfig {
    pub fn new(consul_url: &str, consul_token: &str, community: &str) -> Self {
        Self {
            consul_url: consul_url.to_string(),
            consul_token: consul_token.to_string(),
            community: community.to_string(),
        }
    }

    pub fn wait_or_get(&self, chain: &str) -> Result<serde_json::Value, Error> {
        let keyname = format!("chain/{}/{}", &self.community, &chain);
        let first = Box::new(true);
        let json = self.consul_select(keyname.as_ref(), || {
            if *first {
                info!("chain for config not available in consul yet");
            }
        })?;
        info!("chain for {:?} config available in consul now", chain);
        Ok(json)
    }

    pub fn create_contract_abi(&self, contract_name: &str) -> Result<String, Error> {
        let keyname = format!("chain/{}/{}", &self.community, contract_name);
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
        let config = Config::new(Some(&self.consul_url), Some(&self.consul_token)).unwrap();
        let client = Client::new(config);
        let one_sec = time::Duration::from_secs(1);

        loop {
            if let Ok((Some(keypair), _)) = client.get(keyname, None) {
                let config = decode(&keypair.Value)?;
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

    pub fn watch_for_config_deletion(&self) {
        let url = self.consul_url.clone();
        let token = self.consul_token.clone();
        let community = self.community.clone();
        let chains = vec!["homechain", "sidechain"];
        thread::spawn(move || {
            let config = Config::new(Some(&url), Some(&token)).unwrap();
            let client = Client::new(config);
            let one_sec = time::Duration::from_secs(1);
            let mut contract_addresses = HashMap::new();

            loop {
                for chain in chains.iter() {
                    let keyname = format!("chain/{}/{}", &community, &chain);

                    if let Ok((Some(keypair), _)) = client.get(&keyname, None) {
                        contract_addresses.entry(chain).or_insert_with(|| keypair.Value.clone());
                        let val = &contract_addresses[chain];

                        if &keypair.Value != val {
                            info!("config change detected, exiting...");
                            process::exit(1);
                        } else {
                            thread::sleep(one_sec);
                        }
                    } else {
                        info!("config change detected, exiting...");
                        process::exit(1);
                    }
                }
            }
        });
    }
}
