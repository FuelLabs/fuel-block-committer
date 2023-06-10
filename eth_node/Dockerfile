FROM alpine:3.18 AS fetcher
RUN apk add --no-cache git \
    && git clone --no-checkout https://github.com/FuelLabs/fuel-v2-contracts \
    && cd fuel-v2-contracts \
    && git checkout a83444964db35cc1b93ee7a81d5f47d771083966

FROM alpine:3.18
RUN apk add --no-cache nodejs npm bash

COPY --from=fetcher fuel-v2-contracts /fuel-v2-contracts/

WORKDIR /fuel-v2-contracts

RUN npm install && npm run compile &> /dev/null && npm cache clean --force

COPY hardhat.config.ts run.sh health_check.sh ./

CMD ["./run.sh"]
