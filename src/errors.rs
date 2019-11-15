use actix_web::{http, HttpResponse, ResponseError};
/// # Rationale
/// Relay defines multiple failure types to have an easy way to delineate
/// between errors we may want to emit during verbose logging or to allow the
/// user to filter errors on which "unit" an error arises from.
use failure_derive::Fail;

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

    #[fail(display = "Unable to build transaction: {}", _0)]
    CouldNotBuildTransaction(String),
}

#[derive(Fail, Debug)]
pub enum EndpointError {
    #[fail(display = "invalid chain: {}.", _0)]
    BadChain(String),

    #[fail(display = "invalid transaction hash: {}.", _0)]
    BadTransactionHash(String),

    #[fail(display = "receiver closed.")]
    UnableToSend,

    #[fail(display = "unable to get relay status.")]
    UnableToGetStatus,

    #[fail(display = "unable to get nectar balances.")]
    UnableToGetBalances,

    #[fail(display = "timeout")]
    Timeout,
}

impl ResponseError for EndpointError {
    fn error_response(&self) -> HttpResponse {
        match *self {
            EndpointError::BadChain(_) => HttpResponse::new(http::StatusCode::BAD_REQUEST),
            EndpointError::BadTransactionHash(_) => HttpResponse::new(http::StatusCode::BAD_REQUEST),
            EndpointError::UnableToSend => HttpResponse::new(http::StatusCode::INTERNAL_SERVER_ERROR),
            EndpointError::UnableToGetStatus => HttpResponse::new(http::StatusCode::INTERNAL_SERVER_ERROR),
            EndpointError::UnableToGetBalances => HttpResponse::new(http::StatusCode::INTERNAL_SERVER_ERROR),
            EndpointError::Timeout => HttpResponse::new(http::StatusCode::REQUEST_TIMEOUT),
        }
    }
}

/// ConfigError defines errors arising from an application misconfiguration,
/// they should *only* be triggered at startup.
#[derive(Fail, Debug, PartialEq, Clone)]
pub enum ConfigError {
    #[fail(display = "unable to fetch configuration from consul")]
    ConsulError,

    #[fail(display = "invalid config file path")]
    InvalidConfigFilePath,

    #[fail(display = "invalid confirmations, must be less than anchor frequency")]
    InvalidConfirmations,

    #[fail(display = "invalid anchor frequency, must be non-zero")]
    InvalidAnchorFrequency,

    #[fail(display = "invalid lookback interval, must be below {}", _0)]
    InvalidLookbackInterval(u64),

    #[fail(display = "not such keyfiles directory exists")]
    InvalidKeydir,

    #[fail(display = "invalid port, must be between 0 and 65535")]
    InvalidPort,
}
