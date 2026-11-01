# Multi-arch: the official rust and debian images publish linux/amd64 and
# linux/arm64. `docker buildx build --platform linux/amd64,linux/arm64` builds
# each platform from this file, natively or under QEMU. No arch-specific packages.

FROM rust:1-bookworm AS builder
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --release --locked -p njt-gtfs-proxy

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 65532 --home-dir /var/lib/njt-gtfs-proxy --create-home njt
COPY --from=builder /src/target/release/njt-gtfs-proxy /usr/local/bin/njt-gtfs-proxy
USER 65532:65532
EXPOSE 8080
# A published container port cannot reach 127.0.0.1 inside the container.
# Set NJT_PROXY_ACCESS_KEY before you publish this port; the binary default
# outside Docker remains 127.0.0.1.
ENV NJT_PROXY_BIND=0.0.0.0:8080
ENTRYPOINT ["njt-gtfs-proxy"]
