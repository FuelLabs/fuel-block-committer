#!/usr/bin/env bash

set -e
script_location="$(readlink -f "$(dirname "$0")")"

workspace_cargo_manifest="$script_location/Cargo.toml"

cargo test --manifest-path "$workspace_cargo_manifest" --workspace --exclude e2e

# So that we may have a binary in `target/release`
cargo build --release --manifest-path "$workspace_cargo_manifest" --bin fuel-block-committer
PATH="$script_location/target/release:$PATH" cargo test --manifest-path "$workspace_cargo_manifest" --package e2e -- --test-threads=1
