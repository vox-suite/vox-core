FROM rust:1.91-slim-bookworm AS builder
WORKDIR /app/vox-core

RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*

COPY vox-core/Cargo.toml vox-core/Cargo.lock ./
COPY vox-core/migrations ./migrations
COPY vox-core/contracts ./contracts
COPY vox-core/services ./services
COPY vox-core/src ./src
COPY vox-connections /app/vox-connections
COPY vox-shared /app/vox-shared

RUN cargo build --release --locked --bins

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates curl && rm -rf /var/lib/apt/lists/*

RUN useradd -m -u 1000 -U vox
USER vox

COPY --from=builder --chown=vox:vox /app/vox-core/target/release/vox-core-api /usr/local/bin/vox-core-api
COPY --from=builder --chown=vox:vox /app/vox-core/target/release/vox-core-worker /usr/local/bin/vox-core-worker
COPY --from=builder --chown=vox:vox /app/vox-core/target/release/vox-core-defaults /usr/local/bin/vox-core-defaults

EXPOSE 3001
CMD ["/usr/local/bin/vox-core-api"]
