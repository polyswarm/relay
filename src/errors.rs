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
        InvalidAddress(addr: String) {
            description("invalid address"),
            display("invalid address: '{}'", addr),
        }
    }
}
