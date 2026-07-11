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

# Runtime deps for yt-dlp trailer fetching:
#  - python3: the arch-agnostic `yt-dlp` release is a Python zipapp.
#  - ffmpeg: muxing/remuxing downloaded streams.
#  - deno: JS runtime yt-dlp needs for YouTube's nsig/JS challenges, and the
#    runtime for the bgutil PO-token provider in script mode (below).
#  - bgutil PO-token provider (plugin + server source): lets yt-dlp mint
#    proof-of-origin tokens IN-PROCESS via deno (script mode, no sidecar) so
#    YouTube stops blocking datacenter requests. The plugin (client) and server
#    source are pinned to the same version; `deno install` pre-fetches the
#    server's deps so token generation works at runtime.
ENV POT_VERSION=1.3.1
RUN apt-get update && apt-get install -y --no-install-recommends \
	ca-certificates \
	ffmpeg \
	python3 \
	curl \
	unzip \
	&& curl -fsSL https://github.com/yt-dlp/yt-dlp-nightly-builds/releases/latest/download/yt-dlp \
	-o /usr/local/bin/yt-dlp \
	&& chmod +x /usr/local/bin/yt-dlp \
	&& DENO_ARCH="$(uname -m | sed -e 's/x86_64/x86_64-unknown-linux-gnu/' -e 's/aarch64/aarch64-unknown-linux-gnu/')" \
	&& curl -fsSL "https://github.com/denoland/deno/releases/latest/download/deno-${DENO_ARCH}.zip" -o /tmp/deno.zip \
	&& unzip -q /tmp/deno.zip -d /usr/local/bin \
	&& chmod +x /usr/local/bin/deno \
	&& rm /tmp/deno.zip \
	&& mkdir -p /etc/yt-dlp/plugins \
	&& curl -fsSL "https://github.com/Brainicism/bgutil-ytdlp-pot-provider/releases/download/${POT_VERSION}/bgutil-ytdlp-pot-provider.zip" \
	-o /etc/yt-dlp/plugins/bgutil-ytdlp-pot-provider.zip \
	&& curl -fsSL "https://github.com/Brainicism/bgutil-ytdlp-pot-provider/archive/refs/tags/${POT_VERSION}.tar.gz" -o /tmp/pot.tar.gz \
	&& mkdir -p /root/bgutil-ytdlp-pot-provider \
	&& tar -xzf /tmp/pot.tar.gz -C /root/bgutil-ytdlp-pot-provider --strip-components=1 \
	&& rm /tmp/pot.tar.gz \
	&& (cd /root/bgutil-ytdlp-pot-provider/server && deno install --allow-scripts=npm:canvas --frozen) \
	&& apt-get purge -y curl unzip \
	&& apt-get autoremove -y \
	&& rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/cinema /usr/local/bin/cinema
COPY --from=builder /app/frontend/build /app/frontend/build

WORKDIR /app
ENTRYPOINT ["cinema"]
