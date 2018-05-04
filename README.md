# Polyswarm Relay

This acts as a relay between two networks so deposited ETH can be used on a
sidechain. Also, we want to keep balances in sync so we can withdraw ETH. 

## Config

You need to set some configuration for the networks you would like to relay
tokens between.

relay.homechain and relay.sidechain are the two networks you are relaying
between.

```toml
[relay.homechain]
    host = "ws://localhost:8546"
    token = "0x0000000000000000000000000000000000000000"
    relay = "0x0000000000000000000000000000000000000000"

[relay.sidechain]
    host = "ws://localhost:8547"
    token = "0x0000000000000000000000000000000000000000"
    relay = "0x0000000000000000000000000000000000000000"
```

## Usage

```
Polyswarm Relay 0.1.0
Polyswarm Developers <info@polyswarm.io>
Relays ERC20 tokens between two different networks.

USAGE:
    polyswarm-relay <TOML config file>

FLAGS:
    -h, --help       Prints help information
    -V, --version    Prints version information

ARGS:
    <TOML config file>    Configures the two networks we will relay between
```
