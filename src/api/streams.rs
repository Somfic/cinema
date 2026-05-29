use crate::app::{AppContext, Error};
pub use crate::streams::Stream;
use crate::tmdb::{MediaType, TmdbClient};
use crate::torrent::TorrentEngine;
use crate::{streams as streams_mod, subtitles as subtitles_mod};
use cinema_schema::{cinema_api, cinema_events, cinema_type};

#[cinema_type]
pub struct StartStream {
    pub url: String,
    pub local: bool,
}

#[cinema_type]
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
#[cinema_type]
pub struct StreamStatsUpdate {
    pub info_hash: String,
    pub progress_bytes: u64,
    pub total_bytes: u64,
    pub download_speed_mbps: f64,
    pub peers: usize,
    pub finished: bool,
}

#[cinema_type]
pub struct AudioTracks {
    pub tracks: Vec<crate::torrent::AudioTrack>,
    pub subtitles: Vec<crate::torrent::EmbeddedSubtitleTrack>,
    pub duration: Option<f64>,
}

/// Per-file piece-availability bitmap broadcast over WebSocket. 200 buckets,
/// 0..=255 each. Emitted only for files currently being streamed.
#[cinema_type]
pub struct PiecesUpdate {
    pub info_hash: String,
    pub file_idx: i64,
    pub pieces: Vec<u8>,
}

#[cinema_api(namespace = "streams")]
pub trait StreamsApi {
    /// Aggregates available torrent streams for a movie.
    async fn movie(&self, id: i64) -> Result<Vec<Stream>, Error>;

    /// Aggregates available torrent streams for a specific TV episode.
    async fn tv(&self, id: i64, season: i64, episode: i64) -> Result<Vec<Stream>, Error>;

    /// Starts a torrent (idempotent) and returns the playback URL.
    async fn start(&self, info_hash: String, file_idx: i64) -> Result<StartStream, Error>;

    /// Current torrent download stats for a stream.
    async fn stats(&self, info_hash: String) -> Result<StreamStats, Error>;

    /// Per-piece availability bitmap (200 buckets) for a given file in a torrent.
    async fn pieces(&self, info_hash: String, file_idx: i64) -> Result<Vec<u8>, Error>;

    /// Embedded audio + subtitle tracks + duration for a downloaded file.
    async fn audio_tracks(&self, info_hash: String, file_idx: i64) -> Result<AudioTracks, Error>;

    /// Extracts cues from an embedded subtitle track in the source file.
    async fn embedded_subtitles(
        &self,
        info_hash: String,
        file_idx: i64,
        stream_index: i64,
    ) -> Result<Vec<crate::subtitles::SubtitleCue>, Error>;
}

#[cinema_api]
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

    async fn start(&self, info_hash: String, file_idx: i64) -> Result<StartStream, Error> {
        let engine = TorrentEngine::get();
        engine
            .start(&info_hash, file_idx as usize, &self.config)
            .await?;
        let url = format!("/api/stream/{info_hash}/{file_idx}");
        Ok(StartStream { url, local: false })
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
        let engine = TorrentEngine::get();
        let path = engine.file_path(&info_hash, file_idx as usize)?;
        let (tracks, subtitles, duration) = tokio::join!(
            TorrentEngine::audio_tracks(&path),
            TorrentEngine::subtitle_tracks(&path),
            TorrentEngine::probe_duration(&path),
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
        })
    }

    async fn embedded_subtitles(
        &self,
        info_hash: String,
        file_idx: i64,
        stream_index: i64,
    ) -> Result<Vec<crate::subtitles::SubtitleCue>, Error> {
        let engine = TorrentEngine::get();
        let path = engine.file_path(&info_hash, file_idx as usize)?;
        Ok(TorrentEngine::extract_subtitle_cues(&path, stream_index as usize).await)
    }
}

#[cinema_events(namespace = "streams")]
pub trait StreamsEvents {
    /// Per-torrent download stats, emitted every ~2s for each active torrent.
    /// Topic: `streams_stats`. Subscribers filter by `info_hash`.
    fn stats(payload: StreamStatsUpdate);

    /// Per-file piece bitmap, emitted every ~2s for each file currently
    /// being streamed. Topic: `streams_pieces`. Subscribers filter by
    /// `(info_hash, file_idx)`.
    fn pieces(payload: PiecesUpdate);
}
