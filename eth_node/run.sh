#!/bin/bash

for ENV_VAR in MINING_INTERVAL_MIN MINING_INTERVAL_MAX WALLET_KEY URL; do
    if [[ -z "${!ENV_VAR}" ]]; then
        echo "${ENV_VAR} not set!"
        exit 1
    fi
    sed "s/$ENV_VAR/${!ENV_VAR}/g" -i hardhat.config.ts;
done

(npm run node 2>&1 | tee node_err)& 

while true; do
    if grep -q 'WARNING: These accounts, and their private keys, are publicly known.' node_err &> /dev/null ; then
        break
    fi

    sleep 1;
done

yes Y | npm run script-deploy

# part of health checking, so that we don't start the committer before the
# deployment is done
touch /contracts_deployed

# propagate SIGTERM to the node running in the background
trap "trap - SIGTERM && kill -- -$$" SIGINT SIGTERM EXIT

wait
