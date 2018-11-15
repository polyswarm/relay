/// # Rationale
/// Relay defines multiple failure types to have an easy way to delineate
/// between errors we may want to emit during verbose logging or to allow the
/// user to filter errors on which "unit" an error arises from.

/// OperationError defines errors resulting from interaction from, between or
/// with the chains.
#[derive(Fail, Debug, Clone, PartialEq)]
pub enum OperationError {
    #[fail(display = "invalid contract abi")]
    InvalidContractAbi,

    #[fail(display = "invalid address: '{}'", _0)]
    InvalidAddress(String),

    #[fail(display = "could not unlock account '{}', check password", _0)]
    CouldNotUnlockAccount(String),

    #[fail(display = "Unable to get key: {} from consul", _0)]
    CouldNotGetConsulKey(String),

    #[fail(display = "Could not create contract ABI: consul keystore did not contain key \"abi\"")]
    CouldNotCreateContractABI,
}

/// ConfigError defines errors arising from an application misconfiguration,
/// they should *only* be triggered at startup.
#[derive(Fail, Debug, PartialEq, Clone)]
pub enum ConfigError {
    #[fail(display = "invalid config file path")]
    InvalidConfigFilePath,

    #[fail(display = "invalid confirmations, must be less than anchor frequency")]
    InvalidConfirmations,

    #[fail(display = "invalid anchor frequency, must be non-zero")]
    InvalidAnchorFrequency,
}
