use web3::types::H256;

/// Event signature for ERC20 transfers, equals sha3("Transfer(address,address,uint256)")
pub const TRANSFER_EVENT_SIGNATURE: &str = "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef";
/// Event signature for ERC20 transfers, equals sha3("Flush()")
pub const FLUSH_EVENT_SIGNATURE: &str = "0x0c0adcef1ca5bbf843985f21efffb63e6817af6b8dd1f00e7344a8ad05ab2d51";
