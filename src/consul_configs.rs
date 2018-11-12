use base64::decode;
use consul::Client;
use serde_json;
use std::{thread, time};

pub fn wait_or_get(chain: &str, key: &str, consul_url: &str, consul_token: &str, sidechain_name: &str) -> String {
    let client = Client::new(consul_url, consul_token);
    let keystore = client.keystore;
    let one_sec = time::Duration::from_secs(1);
    let mut done = false;
    let mut ret = "".to_string();
    let mut first = true;

    while !done {
        let result = keystore.get_key(format!("chain/{}/{}", &sidechain_name, &chain));

        match result {
            Ok(result) => {
                info!("Chain for {:?} config availible in consol now", chain);
                done = true;
                let result_string = result.unwrap();
                let config = decode(&result_string).unwrap();
                let new_config = String::from_utf8(config).unwrap();
                let json: serde_json::Value = serde_json::from_str(new_config.as_str()).unwrap();
                ret = json[&key]
                    .as_str()
                    .expect(&format!("Key {} doesn't exist in consul", &key))
                    .to_string();
            }
            Err(_) => {
                if first {
                    info!("Chain for {:?} config not availible in consol yet", chain);
                    first = false;
                }
                thread::sleep(one_sec);
                continue;
            }
        };
    }

    ret
}

pub fn create_contract_abi(contract_name: &str, consul_url: &str, consul_token: &str, sidechain_name: &str) -> String {
    let client = Client::new(consul_url, consul_token);
    let keystore = client.keystore;
    let one_sec = time::Duration::from_secs(1);
    let mut done = false;
    let mut ret = "".to_string();

    while !done {
        let result = keystore.get_key(format!("chain/{}/{}", &sidechain_name, &contract_name));

        match result {
            Ok(result) => {
                done = true;
                let result_string = result.unwrap();
                let config = decode(&result_string).unwrap();
                let new_config = String::from_utf8(config).unwrap();
                let json: serde_json::Value = serde_json::from_str(&new_config.as_str()).unwrap();

                ret = serde_json::ser::to_string(&json["abi"]).unwrap();
            }
            Err(_) => {
                info!("ABI json for {:?} not availible in consol yet", contract_name);
                thread::sleep(one_sec);
                continue;
            }
        }
    }

    ret
}
