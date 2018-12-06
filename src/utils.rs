use ethcore_transaction::{Action, Transaction as RawTransactionRequest};
use ethstore::accounts_dir::RootDiskDirectory;
use ethstore::{EthStore, SimpleSecretStore, StoreAccountRef};
use rlp::{Encodable, RlpStream};
use web3::contract::Options;
use web3::types::H160;

// From ethereum_types but not reexported by web3
pub fn clean_0x(s: &str) -> &str {
    if s.starts_with("0x") {
        &s[2..]
    } else {
        s
    }
}

/// Returns a keyfile store of accounts to be used for signing
///
/// # Arguments
///
/// * `keyfile_dir` - directory of keyfiles
pub fn get_store_for_keyfiles(keyfile_dir: &str) -> EthStore {
    let path = match ::std::fs::metadata(keyfile_dir) {
        Ok(_) => keyfile_dir.into(),
        Err(_) => "",
    };
    let dir = RootDiskDirectory::at(path);
    EthStore::open(Box::new(dir)).unwrap()
}

/// Mutates a RlpStream with ready to send transaction data 
///
/// # Arguments
///
/// * `s` - RlpStream to append transaction data to
/// * `fn_data` - Function data from the contract this transaction is being built for
/// * `recipient` - The address the transaction is going to
/// * `account` - The relay account being used to sign this transaction
/// * `store` - Keyfile store that will be used to sign this transaction
/// * `options` - Transaction options
/// * `password` - The password for the given key store
/// * `chain_id` - The network chain id
pub fn build_transaction(
    s: &mut RlpStream,
    fn_data: &Vec<u8>,
    recipient: &H160,
    account: &H160,
    store: &EthStore,
    options: &Options,
    password: &str,
    chain_id: &u64,
) {
    let transaction_request = RawTransactionRequest {
        action: Action::Call(*recipient),
        gas: options.gas.unwrap(),
        gas_price: options.gas_price.unwrap(),
        value: options.value.unwrap(),
        nonce: options.nonce.unwrap(),
        data: fn_data.to_vec(),
    };
    let raw_tx = transaction_request.hash(Some(*chain_id));
    let signed_tx = store
        .sign(&StoreAccountRef::root(*account), &password.into(), &raw_tx)
        .unwrap();
    let tx_with_sig = transaction_request.with_signature(signed_tx, Some(*chain_id));
    tx_with_sig.rlp_append(s);
}
