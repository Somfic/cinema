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

The flake provisions Rust, bun, ffmpeg, yt-dlp, and deno.

### Without Nix

Install manually:

- Rust (stable)
- [bun](https://bun.sh/)
- ffmpeg, yt-dlp, deno (deno is needed by yt-dlp for YouTube's JS challenges)
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
| `CINEMA_MAX_CONCURRENT_PRETRANSCODINGS` | Max concurrent background pretranscodes. ffmpeg + a single GPU is the bottleneck for full transcodes. | `1` |
| `CINEMA_TORRENT_PORT` | Torrent listen port | `6881` |
| `CINEMA_USE_DHT` | Enable DHT for peer discovery | `true` |
| `CINEMA_TORRENT_VALIDATION_TIMEOUT_MS` | Maximum torrent validation timeout. Configure this if Cinema is run on limited hardware. | 30 seconds |
| `CINEMA_FFMPEG_MAX_STARTUP_DURATION_MS` | Maximum ffmpeg startup timeout. Generous because a cold 4K stream can take >10s to buffer the first segment. | 45 seconds |
| `CINEMA_FFMPEG_STARTUP_POLL_INTERVAL_MS` | Interval at which the success of ffmpeg startup is checked. | 100 milliseconds |
| `CINEMA_FFMPEG_HWACCEL` | ffmpeg `-hwaccel` value. `auto` enables HW-accelerated decoding (e.g. V4L2/DRM on RPi 5) with software fallback. Set to `none` to disable. | `auto` |
| `CINEMA_FFMPEG_VIDEO_PRESET` | libx264 preset for transcoded video. Faster presets reduce CPU; on Pi 5 keep at `ultrafast`. | `ultrafast` |
| `CINEMA_FFMPEG_VIDEO_CRF` | libx264 CRF (quality). Higher = faster + lower quality. Try `28`–`30` on Pi 5 for native-4K transcoding. | `23` |

Note: the RPi 5 has no hardware H.264 encoder, so native 4K transcoding remains CPU-bound even with `CINEMA_FFMPEG_HWACCEL=auto`. If real-time playback isn't met, raise `CINEMA_FFMPEG_VIDEO_CRF` (lower quality, faster encode).
