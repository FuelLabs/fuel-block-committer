#!/usr/bin/env bash

set -e
script_location="$(readlink -f "$(dirname "$0")")"

workspace_cargo_manifest="$script_location/Cargo.toml"

# So that we may have a binary in `target/debug`
cargo build --manifest-path "$workspace_cargo_manifest" --bin fuel-block-committer

PATH="$script_location/target/debug:$PATH" cargo test --manifest-path "$workspace_cargo_manifest" --workspace
# PATH="$script_location/target/debug:$PATH" cargo test --manifest-path "$workspace_cargo_manifest" --package e2e -- connecting_to_testnet --nocapture
