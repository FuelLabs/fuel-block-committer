#!/bin/sh

(npm run node 2>&1 | tee node_err)& 

export CONTRACTS_RPC_URL=http://127.0.0.1:8545
export CONTRACTS_DEPLOYER_KEY=0x9e56ccf010fa4073274b8177ccaad46fbaf286645310d03ac9bb6afa922a7c36 

while true; do
    if grep -q 'WARNING: These accounts, and their private keys, are publicly known.' node_err &> /dev/null ; then
        break
    fi

    sleep 1;
done

yes Y | npm run script-deploy

wait
