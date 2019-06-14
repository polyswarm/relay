#!/usr/bin/env bash

set -e

apt-get update
apt-get install -y jq zsh binutils-dev libcurl4-openssl-dev zlib1g-dev libdw-dev libiberty-dev unzip cmake build-essential
rustup update nightly # required for kcov instrumentation
rustup default nightly
cargo install cargo-cov
./collect_coverage.sh --auto
