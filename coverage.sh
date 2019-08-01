#! /bin/bash

set -e

rustup install beta
cargo +beta install cargo-tarpaulin
cargo tarpaulin
