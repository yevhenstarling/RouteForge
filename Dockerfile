# syntax=docker/dockerfile:1

# One image definition for the services of RouteForge, with a target for each (D38):
#
#   docker build --target solver .
#
# The compiler is the one rust-toolchain.toml pins (D10). That file is kept out of the
# build context, because it also names clippy and rustfmt, and rustup would fetch them
# for a build that uses neither. So the version is repeated here: keep the two in step.
ARG RUST_VERSION=1.99.0
ARG DEBIAN_RELEASE=trixie

# cargo-chef splits the build in two. The dependencies change rarely and take minutes;
# the workspace's own crates change with every commit and take seconds. The first half
# becomes a layer of its own, which Docker reuses until Cargo.lock changes.
FROM rust:${RUST_VERSION}-slim-${DEBIAN_RELEASE} AS chef
RUN cargo install cargo-chef --locked --version 0.1.78
WORKDIR /app

# The recipe: every manifest and the lock file, and no source.
FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
# Queries are checked against the metadata in .sqlx/, so the build needs no database
# (D28). .cargo/config.toml says the same; this holds even where that file is not read.
ENV SQLX_OFFLINE=true
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --locked --package routeforge-solver --recipe-path recipe.json
COPY . .
# A service that gets a target below adds its package here and to the line above.
RUN cargo build --release --locked --package routeforge-solver

# What a service runs on: the C library its binary links to, and a user without
# privileges. Nothing else is needed, neither OpenSSL nor CA certificates (D12).
FROM debian:${DEBIAN_RELEASE}-slim AS runtime
RUN useradd --system --no-create-home --uid 10001 routeforge
USER routeforge

FROM runtime AS solver
COPY --from=builder /app/target/release/routeforge-solver /usr/local/bin/
ENTRYPOINT ["routeforge-solver"]
