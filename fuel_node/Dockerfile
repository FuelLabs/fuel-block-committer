ARG fuel_core_version
FROM ghcr.io/fuellabs/fuel-core:${fuel_core_version}

# for healthcheck
RUN apt-get update && apt-get install -y curl --no-install-recommends && apt-get clean && rm -rf /var/lib/apt/lists/*
