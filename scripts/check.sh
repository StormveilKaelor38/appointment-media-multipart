#!/bin/sh
set -eu
cargo test --offline
cargo run --offline --bin appointment-upload -- "$@"

