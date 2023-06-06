#!/bin/bash -e

docker build ./eth_node -t eth_node

cargo test --test e2e
