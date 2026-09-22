FROM rust:1.91-slim-bookworm AS builder
WORKDIR /app

RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock ./
COPY migrations ./migrations
COPY contracts ./contracts
COPY services ./services
COPY src ./src

RUN cargo build --release --locked --bins

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates curl && rm -rf /var/lib/apt/lists/*

RUN useradd -m -u 1000 -U vox
USER vox

COPY --from=builder --chown=vox:vox /app/target/release/vox-core-api /usr/local/bin/vox-core-api
COPY --from=builder --chown=vox:vox /app/target/release/vox-core-worker /usr/local/bin/vox-core-worker

EXPOSE 3001
CMD ["/usr/local/bin/vox-core-api"]
