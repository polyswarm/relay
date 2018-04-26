use web3;
use web3::futures::{Future, Stream};
use web3::types::{Address, Bytes, FilterBuilder, H160, H256};
use ethabi::{Event, EventParam, Hash, ParamType};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;

#[derive(Clone)]
pub struct Relay {
    account: Address,
    password: String,
    homechain: Network,
    sidechain: Network,
}

impl Relay {
    pub fn new(account: &str, password: &str, homechain: Network, sidechain: Network) -> Relay {
        let start = if account.starts_with("0x") { 2 } else { 0 };
        let verifier_account: Address = account[start..40 + start]
            .parse()
            .expect("invalid verifier address.");

        Relay {
            account: verifier_account,
            password: password.to_owned(),
            homechain,
            sidechain,
        }
    }

    pub fn start(&mut self) {
        // Create channels for communicating with homechain network
        let (from_homechain_tx, to_sidechain_rx) = mpsc::channel();
        self.homechain.set_tx(from_homechain_tx);

        // Create channels for communicating with sidechain channel
        let (from_sidechain_tx, to_homechain_rx) = mpsc::channel();
        self.sidechain.set_tx(from_sidechain_tx);

        // homechain listen
        let mut homechain_listen = self.homechain.clone();
        let homechain = thread::spawn(move || {
            homechain_listen.listen();
        });

        // homechain mint
        let homechain_mint = self.homechain.clone();
        let mint_homechain = thread::spawn(move || {
            let mut iter = to_homechain_rx.iter();
            while let Some(message) = iter.next() {
                homechain_mint.mint(message);
            }
        });

        // sidechain listen
        let mut sidechain_listen = self.sidechain.clone();
        let sidechain = thread::spawn(move || {
            sidechain_listen.listen();
        });

        // sidechain mint
        let sidechain_mint = self.sidechain.clone();
        let mint_sidechain = thread::spawn(move || {
            let mut iter = to_sidechain_rx.iter();
            while let Some(message) = iter.next() {
                sidechain_mint.mint(message);
            }
        });

        // No worries about a deadlock. None of these depend on one another.
        homechain.join().unwrap();
        sidechain.join().unwrap();
        mint_sidechain.join().unwrap();
        mint_homechain.join().unwrap();
    }

    pub fn stop(&mut self) {
        self.homechain.cancel();
        self.sidechain.cancel();
    }
}

#[derive(Clone)]
pub struct Network {
    name: String,
    host: String,
    contracts: Contracts,
    run: Arc<Mutex<bool>>,
    tx: Arc<Mutex<Option<mpsc::Sender<String>>>>,
}

impl Network {
    pub fn new(name: &str, host: &str, token: &str, relay: &str) -> Network {
        Network {
            name: name.to_string(),
            host: host.to_string(),
            contracts: Contracts::new(token, relay),
            run: Arc::new(Mutex::new(false)),
            tx: Arc::new(Mutex::new(None)),
        }
    }

    pub fn cancel(&mut self) {
        *self.run.lock().unwrap() = false;
        let mut lock = self.tx.lock().unwrap();
        if let Some(tx) = lock.clone() {
            drop(tx);
        }
        *lock = None;
    }

    pub fn set_tx(&mut self, sender: mpsc::Sender<String>) {
        *self.tx.lock().unwrap() = Some(sender);
    }

    pub fn mint(&self, sender: String) {
        println!("{:?}", sender);
    }

    pub fn listen(&mut self) {
        *self.run.lock().unwrap() = true;
        let contracts = &self.contracts;
        // Contractsall logs on the specified address
        let token = vec![contracts.token_addr];

        // Contractslogs on transfer topic
        let event_prototype = Contracts::generate_topic_filter();

        // Create filter on our subscription
        let fb: FilterBuilder = FilterBuilder::default().address(token);

        // Start listening to events
        // Open Websocket and create RPC conn
        let (_eloop, ws) = web3::transports::WebSocket::new(&self.host).unwrap();
        let web3 = web3::Web3::new(ws.clone());
        let mut sub = web3.eth_subscribe()
            .subscribe_logs(fb.build())
            .wait()
            .unwrap();

        println!("Got subscription id: {:?}", sub.id());

        let arc_run = self.run.clone();
        let arc_tx = self.tx.clone();

        (&mut sub)
        /*
         * Looks like at best, we can kill the subscription after it is cancelled
         * and geth receives an event. 
         */
            .take_while(|_x| {
                let run = arc_run.lock().unwrap().clone();
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
                    if let Some(tx) = arc_tx.lock().unwrap().clone() {
                        // Print the transfer event.
                        let log = format!("{}: Transfer {:?} Nectar from {:?} to {:?} ",
                            &self.name,
                            amount,
                            H160::from(x.topics[1]),
                            H160::from(x.topics[2]));
                        tx.send(log).unwrap();
                    }
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
/// # Contracts
///
/// This struct points to the relay & token contracts on the network. Use it
/// to generate the log filters (eventually)
///
#[derive(Debug, Clone)]
pub struct Contracts {
    // contract address to subscribe to
    token_addr: Address,
    // Relay contract address (Only care about deposits into that addr)
    relay_addr: Address,
}

impl Contracts {
    pub fn new(token: &str, relay: &str) -> Contracts {
        let mut start = match relay.starts_with("0x") {
            true => 2,
            false => 0,
        };
        // Create an H160 address from address
        let token_hex: Address = token[start..40 + start]
            .parse()
            .expect("invalid token address.");

        start = match token.starts_with("0x") {
            true => 2,
            false => 0,
        };
        let relay_hex: Address = relay[start..40 + start]
            .parse()
            .expect("invalid relay address.");

        Contracts {
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
