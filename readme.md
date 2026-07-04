# cinema

*A self-hosted media torrenting server for movies and TV shows.*

![](https://i.imgur.com/uLk4rJn.jpeg)

![](https://i.imgur.com/mNCzLNB.jpeg)

![](https://i.imgur.com/CCBiQYU.jpeg)

## Docker

```sh
docker run -e CINEMA_TMDB_API_KEY=your_api_key -v ./data:/app/data -p 3000:3000 -p 6881:6881 ghcr.io/somfic/cinema
```

For reliable trailers, use Compose so the PO-token provider runs alongside cinema
(see [Trailers](#trailers)):

```sh
CINEMA_TMDB_API_KEY=your_api_key docker compose up -d
```

### Environment variables

| Variable | Description | Default |
|---|---|---|
| `CINEMA_HOST` | Bind address | `0.0.0.0` |
| `CINEMA_PORT` | HTTP port | `3000` |
| `CINEMA_DATA_DIR` | Data directory path | `./data/` |
| `CINEMA_DATABASE_URL` | Database connection string (sqlite/postgres/mysql) | SQLite in data dir |
| `CINEMA_CONFIG` | Config file path | `cinema.toml` |
| `CINEMA_TMDB_API_KEY` | TMDB API key (required) | |
| `CINEMA_YTDLP_POT_BASE_URL` | bgutil PO-token provider URL, so YouTube stops blocking datacenter requests (see [Trailers](#trailers)) | unset |
| `CINEMA_YTDLP_COOKIES` | Path to a `cookies.txt` for yt-dlp (overrides the uploaded one) | unset |
| `CINEMA_YTDLP_COOKIES_FROM_BROWSER` | Browser to read cookies from when no cookies file is set | `chrome` |
| `CINEMA_STREAM_SOURCES` | Comma-separated stream source URLs | `https://torrentio.strem.fun` |
| `CINEMA_SUBTITLE_LANGUAGES` | Comma-separated subtitle languages | `en` |
| `CINEMA_MAX_CONCURRENT_DOWNLOADS` | Max concurrent background downloads | `2` |
| `CINEMA_TORRENT_PORT` | Torrent listen port | `6881` |
| `CINEMA_USE_DHT` | Enable DHT for peer discovery | `true` |
| `CINEMA_TORRENT_VALIDATION_TIMEOUT_MS` | Maximum torrent validation timeout. Configure this if Cinema is run on a limited hardware. | 30 seconds |
| `CINEMA_FFMPEG_MAX_STARTUP_DURATION_MS` | Maximum ffmpeg startup timeout. Configure this if Cinema is run on a limited hardware. | 10 seconds |
| `CINEMA_FFMPEG_STARTUP_POLL_INTERVAL_MS` | Interval at which the success of ffmpeg startup is checked. | 100 milliseconds |
| `CINEMA_FFMPEG_HWACCEL` | ffmpeg `-hwaccel` value. `auto` enables HW-accelerated decoding (e.g. V4L2/DRM on RPi 5) with software fallback. Set to `none` to disable. | `auto` |
| `CINEMA_FFMPEG_VIDEO_PRESET` | libx264 preset for transcoded video. Faster presets reduce CPU; on Pi 5 keep at `ultrafast`. | `ultrafast` |
| `CINEMA_FFMPEG_VIDEO_CRF` | libx264 CRF (quality). Higher = faster + lower quality. Try `28`–`30` on Pi 5 for native-4K transcoding. | `23` |

Note: the RPi 5 has no hardware H.264 encoder, so native 4K transcoding remains CPU-bound even with `CINEMA_FFMPEG_HWACCEL=auto`. If real-time playback isn't met, raise `CINEMA_FFMPEG_VIDEO_CRF` (lower quality, faster encode).

## Trailers

Trailers are fetched on demand with `yt-dlp` from the YouTube keys TMDB provides.
From a server/datacenter IP YouTube frequently blocks anonymous requests, which is
the usual cause of trailers failing to load. Two layers mitigate this:

1. **PO-token provider (recommended).** `docker compose up` runs the
   [`bgutil-ytdlp-pot-provider`](https://github.com/Brainicism/bgutil-ytdlp-pot-provider)
   sidecar; the matching yt-dlp plugin is baked into the image. Set
   `CINEMA_YTDLP_POT_BASE_URL=http://pot-provider:4416` (already wired in
   `docker-compose.yml`) and yt-dlp attaches a proof-of-origin token automatically.
2. **Cookies.** Upload a `cookies.txt` exported from a signed-in browser under
   Settings → Trailers (stored in the data dir), or point `CINEMA_YTDLP_COOKIES`
   at one. A dedicated throwaway account is recommended.

As a last-resort fallback, if a title has an IMDb id and YouTube resolution still
fails, cinema tries the trailer IMDb hosts (Amazon CDN) via yt-dlp's IMDb
extractor. Coverage is best-effort — IMDb's own endpoints also rate-limit
datacenter IPs — so the PO-token provider above is the more reliable fix.
