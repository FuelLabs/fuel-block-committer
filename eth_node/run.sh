#!/bin/bash

for ENV_VAR in WALLET_KEY URL; do
    if [[ -z "${!ENV_VAR}" ]]; then
        echo "${ENV_VAR} not set!"
        exit 1
    fi
    sed "s#$ENV_VAR#${!ENV_VAR}#g" -i hardhat.config.ts;
done

(npm run node 2>&1 | tee node_err)& 
# propagate SIGTERM to the node running in the background
trap "trap - SIGTERM && kill -- -$$" SIGINT SIGTERM EXIT

while true; do
    if grep -q -F 'Account #0: 0xBCcA9EcB933Db2481111102E73c61C7c7C4e2366 (10000 ETH)' node_err &> /dev/null ; then
        break
    fi

    sleep 1;
done

yes Y | npm run script-deploy

# part of health checking, so that we don't start the committer before the
# deployment is done
touch /contracts_deployed

wait
