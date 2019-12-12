use web3::types::{TransactionReceipt, Log};

#[derive(Clone)]
pub struct Event {
    pub log: Log,
    pub receipt: TransactionReceipt,
}

impl Event {
    pub fn new(log: &Log, receipt: &TransactionReceipt) -> Self {
        Event {
            log: log.clone(),
            receipt: receipt.clone(),
        }
    }
}
