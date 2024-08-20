# Stage 1: Build
FROM lukemathwalker/cargo-chef:latest-rust-1.79 as chef
WORKDIR /build/
# hadolint ignore=DL3008

FROM chef as planner
ENV CARGO_NET_GIT_FETCH_WITH_CLI=true
COPY . .
RUN cargo chef prepare --recipe-path recipe.json --bin fuel-block-committer

FROM chef as builder
COPY --from=planner /build/recipe.json recipe.json
# Build our project dependencies, not our application!
RUN cargo chef cook --release --recipe-path recipe.json --bin fuel-block-committer
# Up to this point, if our dependency tree stays the same,
# all layers should be cached.
COPY . .
RUN cargo build --release --bin fuel-block-committer

# Stage 2: Run
FROM ubuntu:22.04 as run

RUN apt-get update -y \
  && apt-get install -y --no-install-recommends ca-certificates \
  # Clean up
  && apt-get autoremove -y \
  && apt-get clean -y \
  && rm -rf /var/lib/apt/lists/*

WORKDIR /root/

COPY --from=builder /build/target/release/fuel-block-committer .
COPY --from=builder /build/target/release/fuel-block-committer.d .

ENTRYPOINT ["./fuel-block-committer"]
