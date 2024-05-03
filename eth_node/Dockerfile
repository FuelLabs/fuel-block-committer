FROM alpine:3.19.1 AS fetcher
RUN apk add --no-cache git 
RUN git clone --no-checkout https://github.com/FuelLabs/fuel-bridge \
  && cd fuel-bridge \
  && git checkout c23b1f4 \
  && cd packages/solidity-contracts \
  && rm -rf deploy deployments exports test \
  && cd contracts \
  && sed 's/\(BLOCKS_PER_COMMIT_INTERVAL\) = 10800/\1 = 3/g' -i ./fuelchain/FuelChainState.sol \
  && sed 's/\(TIME_TO_FINALIZE\) = 10800/\1 = 1/g' -i ./fuelchain/FuelChainState.sol

FROM alpine:3.19.1
RUN apk add --no-cache nodejs npm bash curl && npm install -g pnpm

COPY --from=fetcher fuel-bridge /fuel-bridge

WORKDIR /fuel-bridge/packages/solidity-contracts

RUN pnpm install && pnpm compile && mkdir deployments

COPY hardhat.config.ts run.sh ./

ENTRYPOINT ["./run.sh"]
