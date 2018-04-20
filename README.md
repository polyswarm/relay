# Polyswarm Bridge

This acts as a relay between two networks so deposited ETH can be used on a sidechain. Also, we want to keep balances in sync so we can withdraw ETH. 

## Config

You need to set some configuration for the networks you would like to bridge. We used
toml to remain consistent with Rust. Fill in the given template before using.

bridge.main and bridge.side are the two networks you are bridging.

We have it intentionally setup with a bridge root element, for further
customization down the road.

```toml
[bridge.main]
    name = "Main"
    host = "ws://localhost:8546"
    token = "0x0000000000000000000000000000000000000000"
    relay = "0x0000000000000000000000000000000000000000"

[bridge.side]
    name = "Side"
    host = "ws://localhost:8547"
    token = "0x0000000000000000000000000000000000000000"
    relay = "0x0000000000000000000000000000000000000000"
```
## Usage

```
Polyswarm Relay Bridge. 0.0.1
Robert Lathrop <rl@polyswarm.io>
Bridges between two contracts on different networks.

USAGE:
    polyswarm-bridge <TOML configuration file>

FLAGS:
    -h, --help       Prints help information
    -V, --version    Prints version information

ARGS:
    <TOML configuration file>    Configures the two networks we will bridge