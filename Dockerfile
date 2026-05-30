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

# Regenerate the schema (backend `_generated.rs` + frontend `lib/schema/*.ts`)
# from the Rust traits/types so the frontend build below sees up-to-date
# bindings. Mirrors the `just schema` recipe.
RUN cargo run --manifest-path ../draad/Cargo.toml -p draad --features codegen --bin draad --quiet -- --rust-only
RUN TS_RS_EXPORT_DIR=/app/target/draad-bindings cargo test --quiet export_bindings
RUN TS_RS_EXPORT_DIR=/app/target/draad-bindings cargo run --manifest-path ../draad/Cargo.toml -p draad --features codegen --bin draad --quiet

RUN bun run --cwd frontend build
RUN cargo build --release

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    ffmpeg \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/cinema /usr/local/bin/cinema
COPY --from=builder /app/frontend/build /app/frontend/build

WORKDIR /app
ENTRYPOINT ["cinema"]
