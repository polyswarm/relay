extern crate web3;
extern crate ethabi;

use web3::futures::{Future, Stream};
use web3::types::{FilterBuilder, H160, H256, Bytes, Address};
use ethabi::{EventParam, Event, ParamType, Hash};

pub struct Network {
    name: String,
    host: String,
    filter: Filter,
}

impl Network {
    pub fn new(name: &str, host: &str, token: &str, relay: &str) -> Network {
        Network {
            name: name.to_string(),
            host: host.to_string(),
            filter: Filter::new(token, relay),
        }
    }

    pub fn listen(&self) {
        let filter = &self.filter;
        // Filter all logs on the specified address
        let token = vec![filter.token_addr];

        // Filter logs on transfer topic
        let event_prototype = Filter::generate_topic_filter();

        // Create filter on our subscription
        let fb: FilterBuilder = FilterBuilder::default()
            .address(token);

        // Start listening to events
        // Open Websocket and create RPC conn
        let (_eloop, ws) = web3::transports::WebSocket::new(&self.host).unwrap();
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
                    && x.topics[2] == H256::from(&filter.relay_addr)
                {
                    // Fold the data to an amount
                    let Bytes(d) = x.data;
                    let amount = d.iter().fold(0, |total:usize, &value| {
                        let t = total << 8;
                        t | value.clone() as usize
                    });

                    // Print the transfer event.
                    println!("{}: Transfer {:?} Nectar from {:?} to {:?} ",
                        &self.name,
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

///
/// # Filter
///
/// This struct points to the relay & token contracts on the network. Use it
/// to generate the log filters (eventually)
///
pub struct Filter {
    // contract address to subscribe to
    token_addr: Address,
    // Relay contract address (Only care about deposits into that addr)
    relay_addr: Address,
}

impl Filter {
    pub fn new(token: &str, relay: &str) -> Filter {
        // Create an H160 address from address
        let token_vec = Filter::from_hex_to_vec(token).unwrap();
        let token_hex: Address = H160::from(&token_vec[..20]);

        let relay_vec = Filter::from_hex_to_vec(relay).unwrap();
        let relay_hex: Address = H160::from(&relay_vec[..20]);

        Filter {
            token_addr: token_hex,
            relay_addr: relay_hex,
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
    /// # Generate Topic Filter
    ///
    /// This generates the topics[0] filter for listening to a Transfer event. 
    /// Once filters work again, this will be a method and generate the whole
    /// (Option<Vec<H256>, Option<Vec<H256>, Option<Vec<H256>, Option<Vec<H256>)
    /// value.
    ///
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
}