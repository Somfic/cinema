use crate::app::{AppContext, Error};
use crate::downloads::TorrentEngine;
use crate::streams::Stream;
use crate::tmdb::{self, MediaType, TmdbClient};
use crate::{streams as streams_mod, subtitles as subtitles_mod};

#[draad::ty]
pub struct StartStream {
    pub url: String,
    pub local: bool,
}

#[draad::ty]
pub struct RemuxSession {
    pub session_id: String,
    pub playlist_url: String,
}

// TODO: replace with just media_id
/// Optional media context for a stream start. When provided, the download
/// row's media metadata is populated synchronously, so the UI doesn't need
/// to wait for the async TMDB resolver.
#[draad::ty]
pub struct StreamMediaContext {
    pub media_type: tmdb::MediaType,
    pub tmdb_id: i64,
    pub title: String,
    pub poster_path: Option<String>,
    #[serde(default)]
    pub season: i32,
    #[serde(default)]
    pub episode: i32,
    pub resolution: Option<String>,
}

#[draad::ty]
pub struct StreamStats {
    pub progress_bytes: u64,
    pub total_bytes: u64,
    pub download_speed_mbps: f64,
    pub peers: usize,
    pub finished: bool,
}

/// Periodic per-torrent stats broadcast over WebSocket. Carries the
/// `info_hash` so subscribers can filter to the stream they care about
/// (a single topic fans out updates for every active torrent)
#[draad::ty]
pub struct StreamStatsUpdate {
    pub info_hash: String,
    pub progress_bytes: u64,
    pub total_bytes: u64,
    pub download_speed_mbps: f64,
    pub peers: usize,
    pub finished: bool,
}

#[draad::ty]
pub struct AudioTracks {
    pub tracks: Vec<crate::downloads::AudioTrack>,
    pub subtitles: Vec<crate::downloads::EmbeddedSubtitleTrack>,
    pub duration: Option<f64>,
    pub chapters: Vec<crate::downloads::Chapter>,
}

/// Per-file piece-availability bitmap broadcast over WebSocket. 200 buckets,
/// 0..=255 each. Emitted only for files currently being streamed.
#[draad::ty]
pub struct PiecesUpdate {
    pub info_hash: String,
    pub file_idx: i64,
    pub pieces: Vec<u8>,
}

#[draad::api(namespace = "streams")]
pub trait StreamsApi {
    /// Aggregates available torrent streams for a movie.
    #[get]
    async fn movie(&self, id: i64) -> Result<Vec<Stream>, Error>;

    /// Aggregates available torrent streams for a specific TV episode.
    #[get]
    async fn tv(&self, id: i64, season: i64, episode: i64) -> Result<Vec<Stream>, Error>;

    /// Starts a torrent (idempotent) and returns the playback URL.
    /// Always creates/updates a `downloads` row so the DB reflects the
    /// active engine state. When `media` is provided, also populates
    /// `download_meta` synchronously.
    #[post]
    async fn start(
        &self,
        info_hash: String,
        file_idx: i32,
        media: Option<StreamMediaContext>,
    ) -> Result<StartStream, Error>;

    /// Reveals the on-disk file for a torrent stream in the server's file
    /// manager. Only meaningful when the server runs on the user's own machine
    /// (the self-hosted local case).
    #[post]
    async fn reveal(&self, info_hash: String, file_idx: i64) -> Result<(), Error>;

    /// Starts an HLS remux/transcode session for a file and returns its
    /// playlist URL (feed to hls.js). Callers stop the previous session first.
    #[post]
    async fn remux(
        &self,
        info_hash: String,
        file_idx: i32,
        audio: i64,
        t: f64,
        only_audio: bool,
    ) -> Result<RemuxSession, Error>;

    /// Current torrent download stats for a stream.
    #[get]
    async fn stats(&self, info_hash: String) -> Result<StreamStats, Error>;

    /// Per-piece availability bitmap (200 buckets) for a given file in a torrent.
    #[get]
    async fn pieces(&self, info_hash: String, file_idx: i64) -> Result<Vec<u8>, Error>;

    /// Embedded audio + subtitle tracks + duration for a downloaded file.
    #[get]
    async fn audio_tracks(&self, info_hash: String, file_idx: i64) -> Result<AudioTracks, Error>;

    /// Extracts cues from an embedded subtitle track in the source file.
    #[get]
    async fn embedded_subtitles(
        &self,
        info_hash: String,
        file_idx: i64,
        stream_index: i64,
    ) -> Result<Vec<crate::subtitles::SubtitleCue>, Error>;
}

/// Reveal a file in the host's file manager — selecting it where the platform
/// supports it, otherwise opening its containing folder. Best-effort and only
/// meaningful when the server shares a desktop with the user.
fn reveal_in_file_manager(path: &std::path::Path) -> Result<(), Error> {
    use std::process::Command;

    #[cfg(target_os = "macos")]
    {
        Command::new("open").arg("-R").arg(path).spawn()?;
    }

    #[cfg(target_os = "windows")]
    {
        Command::new("explorer")
            .arg(format!("/select,{}", path.display()))
            .spawn()?;
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // Prefer FileManager1's ShowItems (selects the file in its folder),
        // falling back to opening the parent directory via xdg-open. The URI
        // must be properly percent-encoded — torrent paths contain spaces and
        // brackets, and an unencoded file:// URI makes Nautilus report
        // "'file' locations are not supported".
        let selected = url::Url::from_file_path(path)
            .ok()
            .map(|uri| {
                Command::new("dbus-send")
                    .args([
                        "--session",
                        "--dest=org.freedesktop.FileManager1",
                        "--type=method_call",
                        "/org/freedesktop/FileManager1",
                        "org.freedesktop.FileManager1.ShowItems",
                    ])
                    .arg(format!("array:string:{uri}"))
                    .arg("string:")
                    .status()
                    .map(|s| s.success())
                    .unwrap_or(false)
            })
            .unwrap_or(false);
        if !selected {
            let dir = path.parent().unwrap_or(path);
            Command::new("xdg-open").arg(dir).spawn()?;
        }
    }

    Ok(())
}

#[draad::api]
impl StreamsApi for AppContext {
    async fn movie(&self, id: i64) -> Result<Vec<Stream>, Error> {
        let tmdb = TmdbClient::new(&self.config, self.http.clone());
        let item = tmdb.details(MediaType::Movie, id).await?;
        let imdb_id = item
            .imdb_id
            .ok_or_else(|| Error::Generic("No IMDB ID found for this movie".into()))?;
        let path = format!("movie/{imdb_id}");
        Ok(streams_mod::aggregate(&self.http, &self.config.stream_sources, &path).await)
    }

    async fn tv(&self, id: i64, season: i64, episode: i64) -> Result<Vec<Stream>, Error> {
        let tmdb = TmdbClient::new(&self.config, self.http.clone());
        let item = tmdb.details(MediaType::Tv, id).await?;
        let imdb_id = item
            .imdb_id
            .ok_or_else(|| Error::Generic("No IMDB ID found for this show".into()))?;
        let path = format!("series/{imdb_id}:{season}:{episode}");
        Ok(streams_mod::aggregate(&self.http, &self.config.stream_sources, &path).await)
    }

    async fn start(
        &self,
        info_hash: String,
        file_idx: i32,
        media: Option<StreamMediaContext>,
    ) -> Result<StartStream, Error> {
        let media = media.map(|m| crate::downloads::types::MediaContext {
            media_type: m.media_type,
            tmdb_id: m.tmdb_id,
            title: m.title,
            poster_path: m.poster_path,
            season: m.season,
            episode: m.episode,
            resolution: m.resolution,
        });
        crate::downloads::types::Download::ensure_download(
            &self.db,
            &self.downloads,
            &info_hash,
            file_idx,
            media.as_ref(),
        )
        .await?;
        let url = format!("/api/stream/{info_hash}/{file_idx}");
        Ok(StartStream { url, local: false })
    }

    async fn reveal(&self, info_hash: String, file_idx: i64) -> Result<(), Error> {
        let engine = TorrentEngine::get();
        let path = engine.file_path(&info_hash, file_idx as usize)?;
        reveal_in_file_manager(&path)
    }

    async fn remux(
        &self,
        info_hash: String,
        file_idx: i32,
        audio: i64,
        t: f64,
        only_audio: bool,
    ) -> Result<RemuxSession, Error> {
        crate::downloads::types::Download::ensure_download(
            &self.db,
            &self.downloads,
            &info_hash,
            file_idx,
            None,
        )
        .await?;
        let (session_id, playlist_url) = crate::hls::start_session(
            &self.storage,
            &self.config,
            crate::hls::HlsSessionStartInput {
                info_hash,
                file_idx: file_idx as usize,
                audio_index: audio as usize,
                start_time: t,
                only_audio,
            },
        )
        .await?;
        Ok(RemuxSession {
            session_id,
            playlist_url,
        })
    }

    async fn stats(&self, info_hash: String) -> Result<StreamStats, Error> {
        let engine = TorrentEngine::get();
        let stats = engine.stats(&info_hash)?;
        let (download_speed_mbps, peers) = match &stats.live {
            Some(live) => (live.download_speed.mbps, live.snapshot.peer_stats.live),
            None => (0.0, 0),
        };
        Ok(StreamStats {
            progress_bytes: stats.progress_bytes,
            total_bytes: stats.total_bytes,
            download_speed_mbps,
            peers,
            finished: stats.finished,
        })
    }

    async fn pieces(&self, info_hash: String, file_idx: i64) -> Result<Vec<u8>, Error> {
        let engine = TorrentEngine::get();
        Ok(engine.piece_map(&info_hash, file_idx as usize, 200)?)
    }

    async fn audio_tracks(&self, info_hash: String, file_idx: i64) -> Result<AudioTracks, Error> {
        // Block until the torrent is loaded and its metadata is known.
        let engine = TorrentEngine::get();
        engine.ensure_torrent(&info_hash, &self.config).await?;

        // Probe through the local HTTP stream route, not the on-disk file: a
        // still-downloading torrent is sparse on disk (missing pieces = holes),
        // so a direct ffprobe fails until the file is complete. The HTTP route
        // serves through the blocking, range-capable reader the transcode uses,
        // so ffprobe gets coherent bytes (and can seek to a trailing moov atom).
        let url = self.stream_url(&info_hash, file_idx);

        let (tracks, subtitles, duration, chapters) = tokio::join!(
            TorrentEngine::audio_tracks(&url),
            TorrentEngine::subtitle_tracks(&url),
            TorrentEngine::probe_duration(&url),
            TorrentEngine::chapters(&url),
        );
        let allowed: Vec<&str> = self
            .config
            .subtitle_languages
            .iter()
            .map(|l| subtitles_mod::to_iso639_2(l))
            .collect();
        let subtitles = subtitles
            .into_iter()
            .filter(|s| {
                s.language
                    .as_deref()
                    .map(|l| allowed.contains(&l))
                    .unwrap_or(true)
            })
            .collect::<Vec<_>>();
        Ok(AudioTracks {
            tracks,
            subtitles,
            duration,
            chapters,
        })
    }

    async fn embedded_subtitles(
        &self,
        info_hash: String,
        file_idx: i64,
        stream_index: i64,
    ) -> Result<Vec<crate::subtitles::SubtitleCue>, Error> {
        let url = self.stream_url(&info_hash, file_idx);
        Ok(TorrentEngine::extract_subtitle_cues(&url, stream_index as usize).await)
    }
}

impl AppContext {
    /// Loopback URL for this file's range-served bytes, so ffprobe/ffmpeg read
    /// through the blocking torrent reader (the [`crate::urls::STREAM`] route)
    /// rather than the sparse on-disk file.
    fn stream_url(&self, info_hash: &str, file_idx: i64) -> String {
        format!(
            "http://127.0.0.1:{}/api/stream/{}/{}",
            self.config.port, info_hash, file_idx
        )
    }
}

#[draad::events(namespace = "streams")]
pub trait StreamsEvents {
    /// Per-torrent download stats, emitted every ~2s for each active torrent.
    /// Topic: `streams_stats`. Subscribers filter by `info_hash`.
    fn stats(payload: StreamStatsUpdate);

    /// Per-file piece bitmap, emitted every ~2s for each file currently
    /// being streamed. Topic: `streams_pieces`. Subscribers filter by
    /// `(info_hash, file_idx)`.
    fn pieces(payload: PiecesUpdate);
}
