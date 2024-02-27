#!/usr/bin/env bash

set -ex

cargo fmt --check
cargo clippy -- -Dwarnings -Dclippy::unwrap_used
cargo test
