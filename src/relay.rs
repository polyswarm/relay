use web3;
use web3::futures::{Future, Stream};
use web3::types::{FilterBuilder, H160, H256, Bytes};

pub struct Relay {
    host: String,
    port: String,
    address: String,
}

impl Relay {
    pub fn new(host: &str, port: &str, address: &str) -> Relay {
        Relay {
            host: String::from(host),
            port: String::from(port),
            address: String::from(address),
        }
    }

    pub fn listen(&self) {
        // Probably should actually check this is a value addr
        // Allocate a string object with a scope of lifetime
        let mut host: String  = self.host.to_owned();
        // append rest of strings
        host.push_str(":");
        host.push_str(&self.port[..]);

        // Allocate address
        let address = from_hex_to_vec(&self.address).unwrap();
        // Create an H160 address from address
        let hex_address = vec![H160::from(&address[..20])];

        println!("Address hash {:?}", hex_address[0]);

        // Filter all logs on the specified address
        let fb: FilterBuilder = FilterBuilder::default().address(hex_address);
        let (_eloop, ws) = web3::transports::WebSocket::new(&host).unwrap();
        let web3 = web3::Web3::new(ws.clone());
        let mut sub = web3.eth_subscribe().subscribe_logs(fb.build()).wait().unwrap();
        
        println!("Got subscription id: {:?}", sub.id());

        (&mut sub)
            .for_each(|x| {
                println!("Got: {:?}", x);
                let Bytes(d) = x.data;
                let text = H256::from(&d[..32]);
                println!("Data: {:?}", text);
                Ok(())
            })
            .wait()
            .unwrap();

        sub.unsubscribe();

        drop(web3);
    }
}

fn from_hex_to_vec(hex: &str) -> Result<Vec<u8>, String> {
    let mut vec: Vec<u8> = vec![];
    for x in 1..(hex.len()/2) {
        let byte = u8::from_str_radix(&hex[2*x..2*x+2], 16);
        match byte {
            Ok(b) => { vec.push(b); },
            Err(_) => { return Err(format!("Failed to convert {}", hex))},
        }
    }
    Ok(vec)
}
