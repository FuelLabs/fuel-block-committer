#!/bin/bash -e

docker build ./eth_node -t doit

cargo test
