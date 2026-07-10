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

ENV SQLX_OFFLINE=true
RUN cargo build --locked --release

RUN bun run --cwd frontend build

FROM debian:bookworm-slim

# yt-dlp nightly
RUN apt-get update && apt-get install -y --no-install-recommends \
	ca-certificates \
	ffmpeg \
	curl \
	&& curl -fsSL https://github.com/yt-dlp/yt-dlp-nightly-builds/releases/latest/download/yt-dlp \
	-o /usr/local/bin/yt-dlp \
	&& chmod +x /usr/local/bin/yt-dlp \
	&& apt-get purge -y curl \
	&& apt-get autoremove -y \
	&& rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/cinema /usr/local/bin/cinema
COPY --from=builder /app/frontend/build /app/frontend/build

WORKDIR /app
ENTRYPOINT ["cinema"]
