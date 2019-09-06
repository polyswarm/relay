#!/usr/bin/env bash

set -e

rustup component add rustfmt
cargo fmt --all -- --check
rustup component add clippy
cargo clippy --all-targets --all-features
