# cinema

A self-hosted media server for movies and TV shows.

## Docker

```sh
docker run -e CINEMA_TMDB_API_KEY=your_api_key -v ./data:/app/data -p 3000:3000 -p 6881:6881 ghcr.io/somfic/cinema
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
| `CINEMA_STREAM_SOURCES` | Comma-separated stream source URLs | `https://torrentio.strem.fun` |
| `CINEMA_SUBTITLE_LANGUAGES` | Comma-separated subtitle languages | `en` |
| `CINEMA_MAX_CONCURRENT_DOWNLOADS` | Max concurrent background downloads | `2` |
| `CINEMA_TORRENT_PORT` | Torrent listen port | `6881` |
| `CINEMA_USE_DHT` | Enable DHT for peer discovery | `true` |
| `CINEMA_FFMPEG_MAX_STARTUP_DURATION_MS` | Maximum ffmpeg startup timeout. Configure this if Cinema is run on a limited hardware. | 10 seconds |
| `CINEMA_FFMPEG_STARTUP_POLL_INTERVAL_MS` | Interval at which the success of ffmpeg startup is checked. | 100 milliseconds |
