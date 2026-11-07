# syntax=docker/dockerfile:1

# --- tile decoder as WebAssembly --------------------------------------------
FROM rust:1-slim-bookworm AS wasm
RUN rustup target add wasm32-unknown-unknown
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target,id=wasm-target \
    cargo build --locked --profile wasm-release --target wasm32-unknown-unknown -p wx-tile-wasm \
 && cp target/wasm32-unknown-unknown/wasm-release/wx_tile_wasm.wasm /wx_tile_wasm.wasm

# --- web client -------------------------------------------------------------
FROM node:26-slim AS web
WORKDIR /app/web
COPY web/package.json web/package-lock.json ./
RUN npm ci
COPY web/ ./
# The client falls back to its TypeScript decoder if this file is absent.
COPY --from=wasm /wx_tile_wasm.wasm ./public/wasm/wx_tile_wasm.wasm
RUN npm run build

# --- server -----------------------------------------------------------------
FROM rust:1-slim-bookworm AS server
# cmake and a C toolchain build the TLS and zstd libraries the server links.
RUN apt-get update \
 && apt-get install -y --no-install-recommends build-essential cmake pkg-config \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY migrations ./migrations
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target \
    cargo build --release --locked -p wx-server \
 && cp target/release/wx-server /usr/local/bin/wx-server

# --- runtime ----------------------------------------------------------------
FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --create-home --home-dir /app wx \
 && mkdir -p /data && chown wx /data
COPY --from=server /usr/local/bin/wx-server /usr/local/bin/wx-server
COPY --from=web /app/web/dist /app/web/dist
USER wx
WORKDIR /app
ENV BIND_ADDR=0.0.0.0:8080 \
    WEB_DIST=/app/web/dist \
    STORE_URL=file:///data/store \
    BASEMAP_PATH=/data/basemap/world.pmtiles
EXPOSE 8080
CMD ["wx-server"]
