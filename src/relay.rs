extern crate web3;
extern crate ethabi;

use web3::futures::{Future, Stream};
use web3::types::{FilterBuilder, H160, H256, Bytes, Address};
use web3::api::{SubscriptionId};
use web3::Web3;
use web3::transports::WebSocket;
use ethabi::{EventParam, Event, ParamType, Hash};
use std::sync::{mpsc, Arc, Mutex};
use std::cell;

// pub struct Bridge {
//     main: Network,
//     side: Network,
// }

// impl Bridge {
//     fn new(main: Network, side: Network) -> Bridge {
//         Bridge {
//             main,
//             side,
//         }
//     }

//     fn start() {
//         (main_tx, main_rx) = sync::mpsc::channel();

//         thread.spawn(move || {
//             let mut iter = rx.iter();
//             while let Some(message) = iter.next() {
                
//             }

//         });
//     }


// }
#[derive(Clone)]
pub struct Network {
    name: String,
    host: String,
    contracts: Contracts,
    run: Arc<Mutex<cell::RefCell<bool>>>,
    tx: Option<mpsc::Sender<String>>,
}

impl Network {
    pub fn new(name: &str, host: &str, token: &str, relay: &str) -> Network {
        Network {
            name: name.to_string(),
            host: host.to_string(),
            contracts: Contracts::new(token, relay),
            run: Arc::new(Mutex::new(cell::RefCell::new(false))),
            tx: None,
        }
    }

    pub fn cancel(&mut self) {
        println!("Cancelling...");
        self.run.lock().unwrap().replace(false);
    }

    pub fn set_tx(&mut self, sender: mpsc::Sender<String>) {
        self.tx = Some(sender);
    }

    pub fn mint(&self, sender: Address) {
        println!("Sending NCT to {:?}", sender);
    }

    pub fn listen(&mut self) {
        {
            self.run.lock().unwrap().replace(true);
        }
        let contracts = &self.contracts;
        // Contractsall logs on the specified address
        let token = vec![contracts.token_addr];

        // Contractslogs on transfer topic
        let event_prototype = Contracts::generate_topic_filter();

        // Create filter on our subscription
        let fb: FilterBuilder = FilterBuilder::default()
            .address(token);

        // Start listening to events
        // Open Websocket and create RPC conn
        let (_eloop, ws) = web3::transports::WebSocket::new(&self.host).unwrap();
        let web3 = web3::Web3::new(ws.clone());
        let mut sub = web3.eth_subscribe().subscribe_logs(fb.build()).wait().unwrap();

        println!("Got subscription id: {:?}", sub.id());

        let arc_run = self.run.clone();

        (&mut sub)
        /*
         * Looks like at best, we can kill the subscription after it is cancelled
         * and geth receives an event. 
         */
            .take_while(|_x| {
                let lock = arc_run.lock().unwrap();
                let run = lock.borrow().clone();
                Ok(run)
            })
            .for_each(|x| {
                /*
                 * Unfortunately, actually putting a topic filter on the
                 * subscribe_logs does not work.
                 */
                if x.topics[0] == event_prototype
                    && x.topics[2] == H256::from(&contracts.relay_addr)
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

        println!("Ended it.");
    }
}

///
/// # Contracts
///
/// This struct points to the relay & token contracts on the network. Use it
/// to generate the log filters (eventually)
///
#[derive(Debug, Clone)]
pub struct Contracts{
    // contract address to subscribe to
    token_addr: Address,
    // Relay contract address (Only care about deposits into that addr)
    relay_addr: Address,
}

impl Contracts{
    pub fn new(token: &str, relay: &str) -> Contracts{
        // Create an H160 address from address
        let token_hex: Address = H160::custom_from(token);

        let relay_hex: Address = H160::custom_from(relay);

        Contracts{
            token_addr: token_hex,
            relay_addr: relay_hex,
        }
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

///
/// Unfortunately, I can't implement normal From on H160 for two reasons. First,
/// neither exists in my scope. So I create a new From. But, using fn from 
/// conflicts with From<&'static str>::from, so I had to change the name a bit.
///
trait From<T> {
     fn custom_from (T) -> Self;
}


impl<'a> From<&'a str> for H160 {
    ///
    /// # custom_from
    ///
    /// This converts a hex string to an H160, representing the
    /// hex value.
    ///
    fn custom_from(hex: &'a str) -> Self {
        let mut array: [u8; 20] = [0; 20];
        let start = match hex.starts_with("0x") {
            true => 1,
            false => 0
        };
        for x in start..21 {
            let byte = u8::from_str_radix(&hex[2*x..2*x+2], 16);
            match byte {
                Ok(b) => { array[x-1] = b; },
                Err(_) => { panic!(format!("Failed to convert {}", hex))},
            }
        }
        H160::from(array)
    }
}