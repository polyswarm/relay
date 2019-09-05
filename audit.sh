#!/usr/bin/env bash

set -e

cargo install --force cargo-audit
cargo generate-lockfile
cargo audit
