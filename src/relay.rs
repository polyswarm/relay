extern crate web3;
extern crate ethabi;

use web3::futures::{Future, Stream};
use web3::types::{FilterBuilder, H160, H256, Bytes, Address};
use ethabi::{EventParam, Event, ParamType, Hash};

///
/// # Relay
///
/// This struct represents a the Relay contract on a network.
///
pub struct Relay {
    // host for websocket connection
    host: String,
    // port to listen on
    port: String,
    // contract address to subscribe to
    address: Address,
    // Relay contract address (Only care about deposits into that addr)
    to_address: Address,
}

impl Relay {
    pub fn new(host: &str, port: &str, address: &str, to_address: &str) -> Relay {
        // Create an H160 address from address
        let vec_address = Relay::from_hex_to_vec(address).unwrap();
        let hex_address: Address = H160::from(&vec_address[..20]);

        let vec_to_address = Relay::from_hex_to_vec(to_address).unwrap();
        let hex_to_address: Address = H160::from(&vec_to_address[..20]);

        Relay {
            host: String::from(host),
            port: String::from(port),
            address: hex_address,
            to_address: hex_to_address,
        }
    }

    ///
    /// # From Hex To Vec
    ///
    /// This converts a hex string to a vector of u8 values, representing the
    /// hex value. (Skips the 0x, which it expects)
    ///
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

    fn generate_topic_filter() -> Hash {
        // event Transfer(address indexed from, address indexed to, uint256 value)
        let from = EventParam {
            name: "from".to_string(),
            kind: ParamType::Address,
            indexed: true,
        };

        let to = EventParam {
            name: "to".to_string(),
            kind: ParamType::Address,
            indexed: true,
        };

        let value = EventParam {
            name: "value".to_string(),
            kind: ParamType::Uint(256),
            indexed: false,
        };

        let transfer_event = Event {
            name: "Transfer".to_string(),
            inputs: vec![from, to, value],
            anonymous: false,
        };
        transfer_event.signature()
    }

    pub fn listen(&self) {
        let host = self.get_host();

        // Filter all logs on the specified address
        let addresses = vec![self.address];

        // Filter logs on transfer topic
        let event_prototype = Relay::generate_topic_filter();

        println!("Signature {:?}", event_prototype);

        // Create filter on our subscription
        let fb: FilterBuilder = FilterBuilder::default()
            .address(addresses);

        // Start listening to events
        // Open Websocket and create RPC conn
        let (_eloop, ws) = web3::transports::WebSocket::new(&host).unwrap();
        let web3 = web3::Web3::new(ws.clone());
        let mut sub = web3.eth_subscribe().subscribe_logs(fb.build()).wait().unwrap();
        
        println!("Got subscription id: {:?}", sub.id());

        (&mut sub)
            .for_each(|x| {
                /*
                 * Unfortunately, actually putting a topic filter on the 
                 * subscribe_logs does not work.
                 */
                if x.topics[0] == event_prototype 
                    && x.topics[2] == H256::from(&self.to_address) 
                {
                    // Fold the data to an amount
                    let Bytes(d) = x.data;
                    let amount = d.iter().fold(0, |total:usize, &value| {
                        let t = total << 8;
                        t | value.clone() as usize
                    });

                    // Print the transfer event.
                    println!("Transfer {:?} Nectar from {:?} to {:?} ", 
                        amount,
                        H160::from(x.topics[1]),
                        H160::from(x.topics[2]));
                }
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