use std::fmt;

/// # Rationale
/// Relay defines multiple failure types to have an easy way to delineate
/// between errors we may want to emit during verbose logging or to allow the
/// user to filter errors on which "unit" an error arises from.

/// OperationError defines errors resulting from interaction from, between or
/// with the chains.
#[derive(Fail, Debug, Clone, PartialEq)]
pub enum OperationError {
    InvalidContractAbi,
    InvalidAddress(String),
    CouldNotUnlockAccount(::web3::types::Address),
}

impl fmt::Display for OperationError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            OperationError::InvalidContractAbi => {
                write!(f, "Invalid contract ABI")
            },
            OperationError::InvalidAddress(address) => {
                write!(f, "invalid address {}", address)
            },
            OperationError::CouldNotUnlockAccount(address) => {
                write!(f, "could not unlock account '{}', check password", address)
            },
        }
    }
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
    InvalidAnchorFrequency
}
