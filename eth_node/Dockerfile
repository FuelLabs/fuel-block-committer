FROM alpine:3.18

RUN apk add --no-cache nodejs git npm

RUN git clone --depth 1 https://github.com/FuelLabs/fuel-v2-contracts

WORKDIR fuel-v2-contracts

RUN npm install && npm run compile &> /dev/null

COPY run.sh .

CMD ["./run.sh"]
