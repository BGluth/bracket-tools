#!/usr/bin/env sh
# Checks the crates that must stay wasm32-clean: the SDK without its sled
# backend, the scheduler core crate, and the browser/desktop app. Needs `rustup target add wasm32-unknown-unknown`.
set -eu
cd "$(dirname "$0")/.."
cargo check --target wasm32-unknown-unknown -p bracket-tools-startgg --no-default-features
cargo check --target wasm32-unknown-unknown -p bracket-tools-scheduler-core
cargo check --target wasm32-unknown-unknown -p bracket-tools-app
