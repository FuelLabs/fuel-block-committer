import type { HardhatUserConfig } from 'hardhat/types';
import '@nomicfoundation/hardhat-ethers';
import '@nomicfoundation/hardhat-network-helpers';
import '@nomicfoundation/hardhat-verify';
import '@nomicfoundation/hardhat-chai-matchers';
import '@typechain/hardhat';
import '@openzeppelin/hardhat-upgrades';
import 'hardhat-deploy';
import 'solidity-coverage';

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
  networks: {
    hardhat: {
        accounts: [ { privateKey: "KEY", balance: "10000000000000000000000" } ],
        mining: {
            auto: true,
            interval: 1000,
        },
    },
    localhost: {
        accounts: [ "KEY" ],
        url: "URL",
    },
  },
  typechain: {
    outDir: 'typechain',
  },
};

export default config;
