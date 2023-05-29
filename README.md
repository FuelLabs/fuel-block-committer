# fuels-block-committer

To run the block-commiter in a test environment, the following steps have to be taken:

- start a local ethereum testnet
- clone the [fuel-v2-contracts](https://github.com/FuelLabs/fuel-v2-contracts/tree/master) repository
- set CONTRACTS_DEPLOYER_KEY to 0x9e56ccf010fa4073274b8177ccaad46fbaf286645310d03ac9bb6afa922a7c36
- run `npm ci` then `npm run node` then `npm run script-deploy` from the contracts repo
- adjust the config in to load the FuelChainState contract address and the wallet that was used to deploy it
