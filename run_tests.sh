#!/bin/bash -e

docker compose up -d
trap 'docker compose down' EXIT

cargo test --test e2e -- --nocapture
