#!/usr/bin/env bash

set -ex

cargo fmt --check
cargo clippy -- -Dwarnings -Dclippy::unwrap_used -Adead_code
cargo test
