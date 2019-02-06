# Polyswarm Relay

[![pipeline status](https://gitlab.polyswarm.io/externalci/relay/badges/feature/coverage-tests/pipeline.svg)](https://gitlab.polyswarm.io/externalci/relay/commits/feature/coverage-tests)
[![coverage report](https://gitlab.polyswarm.io/externalci/relay/badges/feature/coverage-tests/coverage.svg)](https://gitlab.polyswarm.io/externalci/relay/commits/feature/coverage-tests)

This acts as a relay between two networks so deposited ERC20 tokens can be transferred between networks.

## Config

You need to set some configuration for the networks you would like to relay tokens between.

relay.homechain and relay.sidechain are the two networks you are relaying between.

endpoint configures the http endpoint used to force checks of transaction hashes.

```toml
[relay.homechain]
    host = "ws://localhost:8546"
    token = "0x0000000000000000000000000000000000000000"
    relay = "0x0000000000000000000000000000000000000000"

[relay.sidechain]
    host = "ws://localhost:8547"
    token = "0x0000000000000000000000000000000000000000"
    relay = "0x0000000000000000000000000000000000000000"

[endpoint]
    port = 12344
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

## Endpoint

Use the http endpoint to force a scan of an existing transaction that was missed due to downtime.
While relay automatically looks at old transactions to find any that it missed, the range is limited for performance.
The endpoint fills the gap in transactions missed to to long downtimes, by allowing someone to specify a transaction hash with a transfer.

The endpoint only has one route to hit.

**POST** `/[chain]/[transaction hash]`


## Running tests

`relay` needs more tests, including both integration and unit tests.

To run a full test suite, use:

```
$ cargo test all
```

from the repository root

