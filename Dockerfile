FROM rust:1-bookworm AS chef
RUN cargo install cargo-chef
WORKDIR /app

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder

RUN curl -fsSL https://bun.sh/install | bash
ENV PATH="/root/.bun/bin:${PATH}"

COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json

COPY . .
RUN bun install --cwd frontend --trust

# `cargo build` runs `build.rs` which writes `_generated.rs` into OUT_DIR.
# `cargo test export_bindings` produces ts-rs per-type bindings; then a build
# with `CINEMA_EMIT_TS=1` has draad emit the `frontend/src/lib/schema/*.ts`
# files the frontend build below depends on.
ENV TS_RS_EXPORT_DIR=/app/target/draad-bindings
RUN cargo test --quiet export_bindings
RUN CINEMA_EMIT_TS=1 cargo build --release

RUN bun run --cwd frontend build

FROM debian:bookworm-slim

# yt-dlp nightly + the bgutil PO-token provider plugin. The plugin (client side)
# pairs with the `pot-provider` sidecar in docker-compose.yml: it fetches
# proof-of-origin tokens so YouTube stops blocking datacenter requests. Dropping
# the release zip into a yt-dlp plugin dir needs no Python runtime.
# python3 is required: the arch-agnostic `yt-dlp` release is a Python zipapp
# (`#!/usr/bin/env python3`), and the bgutil PO-token plugin is pure Python too.
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    ffmpeg \
    python3 \
    curl \
    && curl -fsSL https://github.com/yt-dlp/yt-dlp-nightly-builds/releases/latest/download/yt-dlp \
    -o /usr/local/bin/yt-dlp \
    && chmod +x /usr/local/bin/yt-dlp \
    && mkdir -p /etc/yt-dlp/plugins \
    && curl -fsSL https://github.com/Brainicism/bgutil-ytdlp-pot-provider/releases/latest/download/bgutil-ytdlp-pot-provider.zip \
    -o /etc/yt-dlp/plugins/bgutil-ytdlp-pot-provider.zip \
    && apt-get purge -y curl \
    && apt-get autoremove -y \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/cinema /usr/local/bin/cinema
COPY --from=builder /app/frontend/build /app/frontend/build

WORKDIR /app
ENTRYPOINT ["cinema"]
