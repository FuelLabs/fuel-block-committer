FROM alpine:3.18 as fetcher

RUN apk add --no-cache curl tar \
    && curl -sSLf https://github.com/FuelLabs/fuel-core/releases/download/v0.18.2/fuel-core-0.18.2-x86_64-unknown-linux-gnu.tar.gz -L -o fuel-core.tar.gz \
    && tar -xvf fuel-core.tar.gz \
    && chmod +x fuel-core-0.18.2-x86_64-unknown-linux-gnu/fuel-core \
    && mv fuel-core-0.18.2-x86_64-unknown-linux-gnu/fuel-core ./fuel-core

FROM ubuntu:22.04
# for healthcheck
RUN apt-get update && apt-get install -y curl --no-install-recommends && apt-get clean && rm -rf /var/lib/apt/lists/*
COPY --from=fetcher fuel-core /root/fuel-core
CMD [ "/root/fuel-core" ]

