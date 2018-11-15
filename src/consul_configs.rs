use base64::decode;
use consul::Client;
use errors::OperationError;
use failure::Error;
use serde_json;
use std::{thread, time};

pub fn wait_or_get(
    chain: &str,
    key: &str,
    consul_url: &str,
    consul_token: &str,
    sidechain_name: &str,
) -> Result<String, Error> {
    let keyname = format!("chain/{}/{}", &sidechain_name, &chain);
    let first = Box::new(true);
    let print_err = || {
        if *first {
            info!("Chain for config not availible in consol yet");
        }
    };
    let json = consul_select(keyname.as_ref(), consul_url, consul_token, print_err)?;

    info!("Chain for {:?} config availible in consol now", chain);

    match json[&key].as_str() {
        Some(result) => Ok(result.to_string()),
        None => Err(OperationError::CouldNotGetConsulKey(key.to_string()).into()),
    }
}

pub fn create_contract_abi(
    contract_name: &str,
    consul_url: &str,
    consul_token: &str,
    sidechain_name: &str,
) -> Result<String, Error> {
    let keyname = format!("chain/{}/{}", &sidechain_name, &contract_name);
    let print_err = || info!("Chain for config not availible in consol yet");
    let json = consul_select(keyname.as_ref(), consul_url, consul_token, print_err)?;

    match json["abi"].as_str() {
        Some(result) => Ok(result.to_string()),
        None => Err(OperationError::CouldNotCreateContractABI.into()),
    }
}

fn consul_select<F>(
    keyname: &str,
    consul_uri: &str,
    consul_token: &str,
    mut print_err: F,
) -> Result<serde_json::Value, Error>
where
    F: FnMut(),
{
    let client = Client::new(consul_uri, consul_token);
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
