#!/usr/bin/env bash

set -e

cargo install --force cargo-audit
cargo audit
