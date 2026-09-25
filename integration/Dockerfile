# syntax=docker/dockerfile:1

# Build stage: compile consoled against Debian 12 (bookworm, glibc 2.36)
# so the binary matches the runtime image below. Building on the CI host
# (e.g. ubuntu-24.04, glibc 2.39) and copying the binary in does not work.
# Toolchain pinned to the crate MSRV (Cargo.toml rust-version).
FROM rust:1.83-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY frontend ./frontend
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked \
    && install -m 755 target/release/consoled /consoled

FROM debian:12-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
        login \
        passwd \
        adduser \
        openssl \
        python3 \
        procps \
        libpam-modules \
        libpam-runtime \
        ca-certificates \
        bash \
    && rm -rf /var/lib/apt/lists/*

COPY --from=build /consoled /usr/local/sbin/consoled
COPY integration/run_tests.py /tests/run_tests.py
COPY integration/entrypoint.sh /entrypoint.sh
RUN chmod 755 /usr/local/sbin/consoled /tests/run_tests.py /entrypoint.sh

ENTRYPOINT ["/entrypoint.sh"]
