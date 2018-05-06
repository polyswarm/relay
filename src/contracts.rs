pub const ERC20_ABI: &str = include_str!("../abi/ERC20.abi");
pub const ERC20_RELAY_ABI: &str = include_str!("../abi/ERC20Relay.abi");

// sha3("Transfer(address,address,uint256)")
pub const TRANSFER_EVENT_SIGNATURE: &str =
    "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef";
