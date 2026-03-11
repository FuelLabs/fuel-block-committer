# Stage 1: Build
FROM lukemathwalker/cargo-chef:latest-rust-1.85-bookworm AS chef
WORKDIR /build/
# hadolint ignore=DL3008

FROM chef AS planner
ENV CARGO_NET_GIT_FETCH_WITH_CLI=true
COPY . .
RUN cargo chef prepare --recipe-path recipe.json --bin fuel-block-committer

FROM chef AS builder
COPY --from=planner /build/recipe.json recipe.json
# Build our project dependencies, not our application!
RUN cargo chef cook --release --recipe-path recipe.json --bin fuel-block-committer
# Up to this point, if our dependency tree stays the same,
# all layers should be cached.
COPY . .
RUN cargo build --release --bin fuel-block-committer

# Stage 2: Run
FROM gcr.io/distroless/cc-debian12:nonroot AS run

COPY --from=builder --chown=nonroot:nonroot /build/target/release/fuel-block-committer /
COPY --from=builder --chown=nonroot:nonroot /build/target/release/fuel-block-committer.d /

ENTRYPOINT ["/fuel-block-committer"]
