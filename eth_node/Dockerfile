FROM alpine:3.18 AS fetcher
RUN apk add --no-cache git 
RUN git clone --no-checkout https://github.com/FuelLabs/fuel-v2-contracts
RUN cd fuel-v2-contracts && git checkout 81b35368764e6f83969e502812e14baa30b20f95
RUN sed 's/\(BLOCKS_PER_COMMIT_INTERVAL\) = 10800/\1 = 3/g' -i fuel-v2-contracts/contracts/fuelchain/FuelChainState.sol
RUN sed 's/\(TIME_TO_FINALIZE\) = 10800/\1 = 1/g' -i fuel-v2-contracts/contracts/fuelchain/FuelChainState.sol

FROM alpine:3.18
RUN apk add --no-cache nodejs npm bash

COPY --from=fetcher fuel-v2-contracts /fuel-v2-contracts/

WORKDIR /fuel-v2-contracts

RUN npm install && npm run compile &> /dev/null && npm cache clean --force

COPY hardhat.config.ts run.sh ./

CMD ["./run.sh"]
