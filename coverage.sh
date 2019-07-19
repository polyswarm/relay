#! /bin/bash

rustup install beta
cargo +beta install cargo-tarpaulin
cargo tarpaulin
