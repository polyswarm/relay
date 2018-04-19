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

    /// 
    /// ## getHost
    ///
    /// This function combines the host, and port joined on a :
    ///
    fn get_host(&self) -> String {
        let host_vec = vec![self.host.as_str(), self.port.as_str()];
        // Allocate a string object with a scope of lifetime
        let host = host_vec.join(":");
        println!("Host: {}", host);
        host
    }

    pub fn listen(&self) {
        let host = self.get_host();

        // Create an H160 address from address
        let address = Relay::from_hex_to_vec(&self.address).unwrap();
        let hex_address = vec![H160::from(&address[..20])];

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

trait Join<T, U> {
    fn join(&self, middle: T) -> U;
}

impl<'a> Join<&'a str, String> for Vec<&'a str> {

    ///
    /// ## Join
    ///
    /// This function joins a vector of &strs and turns it into a single string,
    /// with the given middle: &str between each element.
    /// 
    fn join(&self, middle: &'a str) -> String {
        let mut joined = String::new();
        for i in 0..self.len() {
            joined.push_str(self[i]);
            if i != self.len() -1 {
                joined.push_str(middle);
            }
        };
        joined
    }
}