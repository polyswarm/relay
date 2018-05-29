error_chain!{
    links {
        Web3(::web3::error::Error, ::web3::error::ErrorKind);
    }

    foreign_links {
        Io(::std::io::Error);
        Config(::config::ConfigError);
        Ctrlc(::ctrlc::Error);
    }

    errors {
        CouldNotUnlockAccount(account: ::web3::types::Address) {
            description("could not unlock account, check password"),
            display("could not unlock account '{}', check password", account),
        }

        InvalidConfigFilePath {
            description("invalid config file path"),
            display("invalid config file path"),
        }

        InvalidAnchorFrequency {
            description("invalid anchor frequency, must be non-zero"),
            display("invalid anchor frequency, must be non-zero"),
        }

        InvalidConfirmations {
            description("invalid confirmations, must be less than anchor frequency"),
            display("invalid confirmations, must be less than anchor frequency"),
        }

        InvalidAddress(addr: String) {
            description("invalid address"),
            display("invalid address: '{}'", addr),
        }

        InvalidContractAbi {
            description("invalid contract abi"),
            display("invalid contract abi"),
        }
    }
}
