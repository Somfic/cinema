# cinema

*A self-hosted media torrenting server for movies and TV shows.*

![](https://i.imgur.com/uLk4rJn.jpeg)

![](https://i.imgur.com/mNCzLNB.jpeg)

![](https://i.imgur.com/CCBiQYU.jpeg)

## Deployment

Cinema needs a Postgres database and a [TMDB API key](https://www.themoviedb.org/settings/api).

```sh
docker run -d \
  -e CINEMA_TMDB_API_KEY=your_api_key \
  -e CINEMA_DATABASE_URL=postgres://user:password@host/cinema \
  -v ./data:/app/data \
  -p 3000:3000 \
  -p 6881:6881 \
  ghcr.io/somfic/cinema
```

Port `3000` serves the UI; `6881` is the torrent listen port (doesn't necessarily have to be exposed). `./data` holds cached torrents, transcodes, and trailers.

For a trailer fallback when YouTube blocks the datacenter, run a [trailers-api](https://github.com/Theryston/trailers-api) instance separately and point Cinema at it with `CINEMA_TRAILERS_API_URL`.

## Development

### With Nix

```sh
nix develop
```

The flake provisions Rust, bun, GStreamer, yt-dlp, and deno.

### Without Nix

Install manually:

- Rust (stable)
- [bun](https://bun.sh/)
- GStreamer 1.24+ with the base, good, bad, ugly and libav plugin sets (e.g. `brew install gstreamer`, or the `gstreamer1.0-*` packages plus `libgstreamer1.0-dev` and `libgstreamer-plugins-base1.0-dev`)
- yt-dlp, deno (deno is needed by yt-dlp for YouTube's JS challenges)
- A running Postgres instance (or use docker compose)

`docker-compose.yaml` spins up a local Postgres on port `5434`:

```sh
docker compose up -d
```

That gives you `postgres://root:password@localhost:5434/cinema_db`.

### Run

#### With just

```sh
just install                             # cargo fetch + bun install
export CINEMA_DATABASE_URL=postgres://root:password@localhost:5434/cinema_db
export CINEMA_TMDB_API_KEY=your_api_key
just dev                                 # backend (--dev) + vite side by side
```

`just dev` proxies the UI through the backend at http://localhost:3000. Migrations under `./migrations` run on startup.

Other targets: `just build` (release binary), `just check` (fmt + clippy + frontend typecheck), `just schema` (regenerate the TypeScript schema from Rust types).

#### Manual run
Allow direnv (and install it if you don't have it): `direnv allow .` (needed only the first time)

Run `cargo run` (backend on port 3000) and `bun dev` (frontend on port 5174) in separate terminals.

### Config file

CLI flags and env vars override anything in `cinema.toml` (path via `--config` / `CINEMA_CONFIG`). See `cinema.example.toml` for a starting point.

## Environment variables

| Variable | Description | Default |
|---|---|---|
| `CINEMA_HOST` | Bind address | `0.0.0.0` |
| `CINEMA_PORT` | HTTP port | `3000` |
| `CINEMA_DATA_DIR` | Data directory path | `./data/` |
| `CINEMA_DATABASE_URL` | Postgres connection string (required). Also read from `DATABASE_URL` as a fallback. | |
| `CINEMA_CONFIG` | Config file path | `cinema.toml` |
| `CINEMA_TMDB_API_KEY` | TMDB API key (required) | |
| `CINEMA_TRAILERS_API_URL` | Base URL of a self-hosted [trailers-api](https://github.com/Theryston/trailers-api) used when YouTube fails. Unset disables the fallback. | unset |
| `CINEMA_YTDLP_POT_BASE_URL` | External bgutil PO-token provider URL. Normally unset — the image mints tokens in-process (script mode). Set only to use a separate provider. | unset |
| `CINEMA_YTDLP_COOKIES` | Path to a `cookies.txt` for yt-dlp (overrides the uploaded one) | unset |
| `CINEMA_YTDLP_COOKIES_FROM_BROWSER` | Browser to read cookies from when no cookies file is set. Left unset in server/container deployments (no browser present); the in-process PO token covers the anonymous case. | unset |
| `CINEMA_STREAM_SOURCES` | Comma-separated stream source URLs | `https://torrentio.strem.fun` |
| `CINEMA_SUBTITLE_LANGUAGES` | Comma-separated subtitle languages | `en` |
| `CINEMA_MAX_CONCURRENT_DOWNLOADS` | Max concurrent background downloads | `2` |
| `CINEMA_MAX_CONCURRENT_PRETRANSCODINGS` | Max concurrent background pretranscodes. A single GPU is the bottleneck for full transcodes. Live streams are never limited: a re-encoding stream pauses background pretranscodes to make room. | `1` |
| `CINEMA_TORRENT_PORT` | Torrent listen port | `6881` |
| `CINEMA_USE_DHT` | Enable DHT for peer discovery | `true` |
| `CINEMA_TORRENT_VALIDATION_TIMEOUT_MS` | Maximum torrent validation timeout. Configure this if Cinema is run on limited hardware. | 30 seconds |
| `CINEMA_TRANSCODE_HARDWARE` | Video encoder family: `auto` (first that works: VideoToolbox, NVENC, VA-API, then x264), `none` (x264 only), `nvidia`, `vaapi` or `videotoolbox`. Decoding always uses the best available decoder. | `auto` |
| `CINEMA_TRANSCODE_PRESET` | x264 speed preset, for software encoding. Faster presets reduce CPU but cost quality (`ultrafast` disables deblocking, which shows as blocks); on a Pi 5 use `ultrafast` with `CINEMA_TRANSCODE_MAX_HEIGHT=1080`. | `veryfast` |
| `CINEMA_TRANSCODE_CRF` | Quality target on the CRF scale (also used for NVENC and VA-API). Lower = better quality, more bits; 18 is visually lossless even on a large 4K screen. Raise it on slow hardware. | `18` |
| `CINEMA_TRANSCODE_MAX_HEIGHT` | Re-encoded video is scaled down to at most this height. `0` keeps the source resolution (scaled down only for clients that can't take it, like a non-4K Chromecast). | `0` |

Most playback needs no encoding at all: Cinema plays a file as is when the client supports it, and otherwise repackages it as HLS, re-encoding only the streams the client can't decode (usually just the audio). Note: the RPi 5 has no hardware H.264 encoder, so re-encoding video remains CPU-bound. If real-time playback isn't met, raise `CINEMA_TRANSCODE_CRF` (lower quality, faster encode).
