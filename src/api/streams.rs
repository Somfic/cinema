use crate::app::{AppContext, CinemaError};
use crate::downloads::TorrentEngine;
use crate::streams::Stream;
use crate::tmdb::{MediaType, TmdbClient};
use crate::{streams as streams_mod, subtitles as subtitles_mod};

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
    pub file_idx: i32,
    pub pieces: Vec<u8>,
}

#[draad::api(namespace = "streams")]
pub trait StreamsApi {
    /// Aggregates available torrent streams for a movie.
    #[get]
    async fn movie(&self, id: i64) -> Result<Vec<Stream>, CinemaError>;

    /// Aggregates available torrent streams for a specific TV episode.
    #[get]
    async fn tv(&self, id: i64, season: u32, episode: u32) -> Result<Vec<Stream>, CinemaError>;

    /// Stops a torrent stream. Is equivalent to pausing the download,
    /// but does not require the download id.
    #[post]
    async fn stop(&self, info_hash: String, file_idx: i32) -> Result<(), CinemaError>;

    /// Reveals the on-disk file for a torrent stream in the server's file
    /// manager. Only meaningful when the server runs on the user's own machine
    /// (the self-hosted local case).
    #[post]
    async fn reveal(&self, info_hash: String, file_idx: i32) -> Result<(), CinemaError>;

    /// Starts playback of a file for a client that decodes `client`, doing
    /// as little work as possible: the original file when the client plays
    /// it, otherwise an HLS session that copies every stream it can and
    /// re-encodes the rest. `mode` forces re-encoding (`Enabled`: video and
    /// audio, `OnlyAudio`: audio), and `hdr: false` delivers HDR sources
    /// tone mapped to SDR. The HLS playlist covers the whole file, so seeking
    /// is the player's own business. Callers stop the previous session
    /// first.
    #[post]
    async fn play(
        &self,
        info_hash: String,
        file_idx: i32,
        audio: i32,
        client: crate::transcodings::ClientCapabilities,
        mode: crate::api::watch::TranscodingOption,
        hdr: bool,
    ) -> Result<crate::transcodings::Playback, CinemaError>;

    /// Current torrent download stats for a stream.
    #[get]
    async fn stats(&self, info_hash: String) -> Result<StreamStats, CinemaError>;

    /// Per-piece availability bitmap (200 buckets) for a given file in a torrent.
    #[get]
    async fn pieces(&self, info_hash: String, file_idx: i64) -> Result<Vec<u8>, CinemaError>;

    /// Embedded audio + subtitle tracks + duration for a downloaded file.
    #[get]
    async fn audio_tracks(
        &self,
        info_hash: String,
        file_idx: i64,
    ) -> Result<AudioTracks, CinemaError>;

    /// Extracts cues from an embedded subtitle track in the source file.
    #[get]
    async fn embedded_subtitles(
        &self,
        info_hash: String,
        file_idx: i64,
        stream_index: i64,
    ) -> Result<Vec<crate::subtitles::SubtitleCue>, CinemaError>;
}

/// Reveal a file in the host's file manager — selecting it where the platform
/// supports it, otherwise opening its containing folder. Best-effort and only
/// meaningful when the server shares a desktop with the user.
fn reveal_in_file_manager(path: &std::path::Path) -> Result<(), CinemaError> {
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
    async fn movie(&self, id: i64) -> Result<Vec<Stream>, CinemaError> {
        let tmdb = TmdbClient::new(&self.config, self.http.clone());
        let item = tmdb.details(MediaType::Movie, id, &self.db).await?;
        let imdb_id = item
            .imdb_id
            .ok_or_else(|| CinemaError::Generic("No IMDB ID found for this movie".into()))?;

        let streams = streams_mod::AggregationMediaType::Media {
            tmdb_id: id,
            imdb_id,
        }
        .aggregate(self)
        .await;

        Ok(streams)
    }

    async fn tv(&self, id: i64, season: u32, episode: u32) -> Result<Vec<Stream>, CinemaError> {
        let tmdb = TmdbClient::new(&self.config, self.http.clone());
        let item = tmdb.details(MediaType::Tv, id, &self.db).await?;
        let imdb_id = item
            .imdb_id
            .ok_or_else(|| CinemaError::Generic("No IMDB ID found for this show".into()))?;

        let streams = streams_mod::AggregationMediaType::Tv {
            tmdb_id: id,
            imdb_id,
            season,
            episode,
        }
        .aggregate(self)
        .await;

        Ok(streams)
    }

    async fn stop(&self, info_hash: String, file_idx: i32) -> Result<(), CinemaError> {
        let id = crate::downloads::types::Download::find_id_by_info_hash_and_file_idx(
            &self.db, &info_hash, file_idx,
        )
        .await?;

        let Some(id) = id else {
            return Err(crate::app::CinemaError::NotFound(format!(
                "No download found for {info_hash} ({file_idx})"
            )));
        };

        self.downloads.pause(id).await
    }

    async fn reveal(&self, info_hash: String, file_idx: i32) -> Result<(), CinemaError> {
        let source = crate::downloads::MediaSource::ensure_and_locate(
            &self.downloads,
            &self.storage,
            &info_hash,
            file_idx,
            crate::downloads::DownloadPriority::Stream,
        )
        .await?;
        reveal_in_file_manager(source.path())
    }

    async fn play(
        &self,
        info_hash: String,
        file_idx: i32,
        audio: i32,
        client: crate::transcodings::ClientCapabilities,
        mode: crate::api::watch::TranscodingOption,
        hdr: bool,
    ) -> Result<crate::transcodings::Playback, CinemaError> {
        self.transcodings
            .start_playback(&info_hash, file_idx, audio, client, mode, hdr)
            .await
    }

    async fn stats(&self, info_hash: String) -> Result<StreamStats, CinemaError> {
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

    async fn pieces(&self, info_hash: String, file_idx: i64) -> Result<Vec<u8>, CinemaError> {
        let engine = TorrentEngine::get();
        Ok(engine.piece_map(&(info_hash, file_idx as usize).into(), 200)?)
    }

    async fn audio_tracks(
        &self,
        info_hash: String,
        file_idx: i64,
    ) -> Result<AudioTracks, CinemaError> {
        let info = self.media_info(&info_hash, file_idx).await?;
        let allowed: Vec<&str> = self
            .config
            .subtitle_languages
            .iter()
            .map(|l| subtitles_mod::to_iso639_2(l))
            .collect();
        let tracks = info
            .audio
            .iter()
            .map(|a| crate::downloads::AudioTrack {
                index: a.index,
                stream_index: a.stream_index,
                name: a.title.clone().unwrap_or_else(|| {
                    let channels = match a.channels {
                        1 => "Mono",
                        2 => "Stereo",
                        6 => "5.1",
                        8 => "7.1",
                        _ => "",
                    };
                    format!("{} {channels}", a.codec.to_uppercase())
                        .trim()
                        .to_string()
                }),
                language: a.language.clone(),
                codec: a.codec.clone(),
            })
            .collect();
        let subtitles = info
            .subtitles
            .iter()
            .filter(|s| s.text)
            .filter(|s| {
                s.language
                    .as_deref()
                    .map(|l| allowed.contains(&l))
                    .unwrap_or(true)
            })
            .map(|s| crate::downloads::EmbeddedSubtitleTrack {
                index: s.index,
                stream_index: s.stream_index,
                language: s.language.clone(),
                name: s.title.clone().unwrap_or_else(|| match &s.language {
                    Some(l) => format!("{l} ({})", s.codec.to_uppercase()),
                    None => s.codec.to_uppercase(),
                }),
                codec: s.codec.clone(),
            })
            .collect();
        let chapters = info
            .chapters
            .iter()
            .enumerate()
            .map(|(i, c)| crate::downloads::Chapter {
                start: c.start.as_secs_f64(),
                end: c.end.as_secs_f64(),
                title: c
                    .title
                    .clone()
                    .unwrap_or_else(|| format!("Chapter {}", i + 1)),
            })
            .collect();
        Ok(AudioTracks {
            tracks,
            subtitles,
            duration: info.duration.map(|d| d.as_secs_f64()),
            chapters,
        })
    }

    async fn embedded_subtitles(
        &self,
        info_hash: String,
        file_idx: i64,
        stream_index: i64,
    ) -> Result<Vec<crate::subtitles::SubtitleCue>, CinemaError> {
        self.embedded_subtitle_cues(&info_hash, file_idx, stream_index)
            .await
    }
}

impl AppContext {
    /// The streams, duration and chapters of a torrent file. A file still
    /// downloading is read through the torrent stream, so the probe waits for
    /// the pieces it needs (an MP4 index at the end, say) instead of reading
    /// holes.
    pub(crate) async fn media_info(
        &self,
        info_hash: &str,
        file_idx: i64,
    ) -> Result<std::sync::Arc<media::MediaInfo>, CinemaError> {
        let source = crate::downloads::MediaSource::ensure_and_locate(
            &self.downloads,
            &self.storage,
            info_hash,
            file_idx as i32,
            crate::downloads::DownloadPriority::Stream,
        )
        .await?;
        let input = source.media_input().await?;
        self.transcodings
            .media_info(&format!("{info_hash}/{file_idx}"), &input)
            .await
    }

    /// Cues of an embedded text subtitle track, by its index among the
    /// file's subtitle tracks.
    pub(crate) async fn embedded_subtitle_cues(
        &self,
        info_hash: &str,
        file_idx: i64,
        stream_index: i64,
    ) -> Result<Vec<crate::subtitles::SubtitleCue>, CinemaError> {
        let source = crate::downloads::MediaSource::ensure_and_locate(
            &self.downloads,
            &self.storage,
            info_hash,
            file_idx as i32,
            crate::downloads::DownloadPriority::Stream,
        )
        .await?;
        let cues = media::extract_subtitles(
            &source.media_input().await?,
            stream_index.max(0) as usize,
            std::time::Duration::from_secs(15),
        )
        .await?;
        Ok(cues
            .into_iter()
            .map(|c| crate::subtitles::SubtitleCue {
                start: c.start,
                end: c.end,
                text: c.text,
            })
            .collect())
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
