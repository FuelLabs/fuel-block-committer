import { HardhatUserConfig } from 'hardhat/types';
import '@nomiclabs/hardhat-waffle';
import '@nomiclabs/hardhat-etherscan';
import '@openzeppelin/hardhat-upgrades';
import 'hardhat-typechain';
import 'solidity-coverage';
import 'hardhat-gas-reporter';
import { config as dotEnvConfig } from 'dotenv';

dotEnvConfig();

const config: HardhatUserConfig = {
    defaultNetwork: 'hardhat',
    solidity: {
        compilers: [
            {
                version: '0.8.9',
                settings: {
                    optimizer: {
                        enabled: true,
                        runs: 10000,
                    },
                },
            },
        ],
    },
    mocha: {
        timeout: 180_000,
    },
    networks: {
        hardhat: {
            mining: {
                auto: false,
                interval: [MINING_INTERVAL_MIN, MINING_INTERVAL_MAX]
            },
            accounts: {
                WALLET_KEY,
            },
        },
        custom: {
            accounts: [WALLET_KEY],
            url: URL,
            live: true,
        },
    },
};

export default config;
