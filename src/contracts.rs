/// ABI definition for an ERC20 token
pub const ERC20_ABI: &'static str = include_str!("../abi/ERC20.abi");
/// ABI definition for an ERC20 token relay
pub const ERC20_RELAY_ABI: &'static str = include_str!("../abi/ERC20Relay.abi");

/// Event signature for ERC20 transfers, equals sha3("Transfer(address,address,uint256)")
pub const TRANSFER_EVENT_SIGNATURE: &'static str = "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef";
