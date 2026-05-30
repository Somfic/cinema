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

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    ffmpeg \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/cinema /usr/local/bin/cinema
COPY --from=builder /app/frontend/build /app/frontend/build

WORKDIR /app
ENTRYPOINT ["cinema"]
