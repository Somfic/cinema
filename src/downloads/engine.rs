use std::{
    collections::HashMap,
    sync::{Arc, OnceLock},
};

use librqbit::{
    AddTorrent, AddTorrentOptions, AddTorrentResponse, Api, Session, SessionOptions,
    api::TorrentIdOrHash,
};
use tokio_util::sync::CancellationToken;

use crate::{config::Config, downloads::TorrentFileReader};

static ENGINE: OnceLock<TorrentEngine> = OnceLock::new();

#[derive(Eq, Hash, PartialEq, Clone)]
pub struct EngineKey {
    pub info_hash: String,
    pub file_idx: usize,
}

impl From<(String, usize)> for EngineKey {
    fn from(value: (String, usize)) -> Self {
        Self {
            info_hash: value.0,
            file_idx: value.1,
        }
    }
}

#[draad::ty]
pub struct AudioTrack {
    /// ffmpeg absolute stream index
    pub index: usize,
    /// audio-only index (0, 1, 2...)
    pub stream_index: usize,
    pub name: String,
    pub language: Option<String>,
    pub codec: String,
}

#[draad::ty]
pub struct EmbeddedSubtitleTrack {
    /// ffmpeg absolute stream index
    pub index: usize,
    /// subtitle-only index (0, 1, 2...)
    pub stream_index: usize,
    pub language: Option<String>,
    pub name: String,
    pub codec: String,
}

#[draad::ty]
pub struct Chapter {
    /// chapter start time in seconds
    pub start: f64,
    /// chapter end time in seconds
    pub end: f64,
    pub title: String,
}

/// A (relatively) thick wrapper around [`librqbit::Session`].
/// Only read-only operations are publically available (such as reading the stats),
/// everything else should go through the [download manager](`crate::downloads::Handle`)
pub struct TorrentEngine {
    session: Arc<Session>,
    api: Api,
    http: reqwest::Client,
    cancel: CancellationToken,
    span: tracing::Span,
    /// Keep a FileStream alive per (info_hash, file_idx) to maintain
    /// sequential piece prioritization from librqbit's streaming system.
    stream_handles: tokio::sync::Mutex<HashMap<EngineKey, TorrentFileReader>>,
    validation_locks: std::sync::Mutex<HashMap<String, std::sync::Weak<tokio::sync::Mutex<()>>>>,
}

impl TorrentEngine {
    pub async fn init(ctx: &crate::AppContext) -> crate::app::Result<()> {
        let output_folder = ctx.storage.join("torrents");
        tokio::fs::create_dir_all(&output_folder).await?;

        let cancel = CancellationToken::new();
        let session_cancel = cancel.clone();

        let opts = SessionOptions {
            disable_dht: !ctx.config.use_dht,
            fastresume: true,
            cancellation_token: Some(session_cancel),
            root_span: Some(tracing::Span::current()),
            // We're no good samaritans
            disable_upload: true,
            listen_port_range: None,
            enable_upnp_port_forwarding: false,
            ..Default::default()
        };

        let session = Session::new_with_opts(output_folder, opts)
            .await
            .map_err(|e| {
                crate::app::Error::Generic(format!("Failed to init torrent session: {e}"))
            })?;

        let api = Api::new(session.clone(), None);

        let span = tracing::Span::current();

        ENGINE
            .set(TorrentEngine {
                session,
                api,
                http: ctx.http.clone(),
                cancel,
                span,
                stream_handles: tokio::sync::Mutex::new(HashMap::new()),
                validation_locks: std::sync::Mutex::new(HashMap::new()),
            })
            .map_err(|_| crate::app::Error::Generic("Torrent engine already initialized".into()))?;

        tracing::info!("Torrent engine initialized");
        Ok(())
    }

    pub async fn stream_stats_supervisor(events: crate::Events) {
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(333));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        let engine = Self::get();
        loop {
            interval.tick().await;

            for hash in engine.active_info_hashes() {
                let Ok(stats) = engine.stats(&hash) else {
                    continue;
                };
                let (download_speed_mbps, peers) = match &stats.live {
                    Some(live) => (live.download_speed.mbps, live.snapshot.peer_stats.live),
                    None => (0.0, 0),
                };
                events
                    .streams
                    .emit_stats(&crate::api::streams::StreamStatsUpdate {
                        info_hash: hash,
                        progress_bytes: stats.progress_bytes,
                        total_bytes: stats.total_bytes,
                        download_speed_mbps,
                        peers,
                        finished: stats.finished,
                    });
            }
            // Piece bitmaps for files currently being streamed. Only
            // pushed for active streams (not every file in every
            // torrent), keeping the wire chatter bounded.
            for key in engine.active_streams().await {
                let Ok(pieces) = engine.piece_map(&key, 200) else {
                    continue;
                };
                events
                    .streams
                    .emit_pieces(&crate::api::streams::PiecesUpdate {
                        info_hash: key.info_hash,
                        file_idx: key.file_idx as i32,
                        pieces,
                    });
            }
        }
    }

    pub fn get() -> &'static TorrentEngine {
        ENGINE.get().expect("TorrentEngine not initialized")
    }

    /// Try to fetch the .torrent file from cache services.
    /// Returns the raw bytes if found, None otherwise.
    async fn fetch_torrent_file(&self, info_hash: &str) -> Option<bytes::Bytes> {
        let hash_upper = info_hash.to_uppercase();
        for template in super::TORRENT_CACHES {
            let url = template.replace("{}", &hash_upper);
            match self.http.get(&url).send().await {
                Ok(resp) if resp.status().is_success() => match resp.bytes().await {
                    Ok(bytes) if bytes.len() > 50 && bytes[0] == b'd' => {
                        self.span.in_scope(|| {
                            tracing::info!(info_hash, url, "Fetched .torrent file from cache")
                        });
                        return Some(bytes);
                    }
                    _ => continue,
                },
                _ => continue,
            }
        }
        None
    }

    /// Build a magnet URI with public trackers as fallback.
    fn magnet_url(info_hash: &str) -> String {
        let mut magnet = format!("magnet:?xt=urn:btih:{info_hash}");
        for tracker in super::PUBLIC_TRACKERS {
            magnet.push_str("&tr=");
            magnet.push_str(tracker);
        }
        magnet
    }

    /// Add the torrent to the session if not already present, and await
    /// metadata resolution. Idempotent - returns immediately if already
    /// initialized. Does **not** modify the selected file set; call
    /// [`select_file`] for that.
    pub(super) async fn ensure_torrent(
        &self,
        info_hash: &str,
        config: &Config,
    ) -> crate::app::Result<super::TorrentHandle> {
        // Fast path: torrent already in session
        if let Ok(id) = TorrentIdOrHash::parse(info_hash)
            && let Some(handle) = self.session.get(id)
        {
            return Ok(super::TorrentHandle { managed: handle });
        }

        // Create a one-validation-per-info-hash lock
        let lock = {
            let mut map = self.validation_locks.lock().unwrap();
            map.retain(|_, weak| weak.strong_count() > 0); // Retain only active locks
            match map.get(info_hash).and_then(|weak| weak.upgrade()) {
                Some(existing) => existing,
                None => {
                    let lock = Arc::new(tokio::sync::Mutex::new(()));
                    map.insert(String::from(info_hash), Arc::downgrade(&lock));
                    lock
                }
            }
        };

        // Await the lock. Only one task actully performs the validation
        let _guard = lock.lock().await;

        // Re-check fast path: in case of a race, winner has already added the torrent
        if let Ok(id) = TorrentIdOrHash::parse(info_hash)
            && let Some(handle) = self.session.get(id)
        {
            return Ok(super::TorrentHandle { managed: handle });
        }

        let make_opts = || AddTorrentOptions {
            // Start with no files selected; the caller selects via `select_file`
            only_files: Some(vec![]),
            sub_folder: Some(info_hash.to_string()),
            overwrite: true,
            ..Default::default()
        };

        // Try .torrent file first (has embedded trackers), fall back to magnet
        let response = if let Some(torrent_bytes) = self.fetch_torrent_file(info_hash).await {
            let add = AddTorrent::from_bytes(torrent_bytes);
            match self.session.add_torrent(add, Some(make_opts())).await {
                Ok(resp) => resp,
                Err(e) => {
                    self.span.in_scope(|| {
                        tracing::warn!(
                            info_hash,
                            error = %e,
                            "Failed to add .torrent file, falling back to magnet+DHT"
                        )
                    });
                    let magnet = Self::magnet_url(info_hash);
                    let add = AddTorrent::from_url(magnet);
                    self.session
                        .add_torrent(add, Some(make_opts()))
                        .await
                        .map_err(|e| {
                            crate::app::Error::Generic(format!("Failed to add torrent: {e}"))
                        })?
                }
            }
        } else {
            self.span.in_scope(|| {
                tracing::info!(
                    info_hash,
                    "No cached .torrent file, falling back to magnet+DHT"
                )
            });
            let magnet = Self::magnet_url(info_hash);
            let add = AddTorrent::from_url(magnet);
            self.session
                .add_torrent(add, Some(make_opts()))
                .await
                .map_err(|e| crate::app::Error::Generic(format!("Failed to add torrent: {e}")))?
        };

        let managed = match response {
            AddTorrentResponse::Added(_, handle) => handle,
            AddTorrentResponse::AlreadyManaged(_, handle) => handle,
            AddTorrentResponse::ListOnly(_) => {
                return Err(crate::app::Error::Generic("Torrent was list-only".into()));
            }
        };

        // Wait for metadata + initial check, but don't block forever
        let init_fut = managed.wait_until_initialized();

        tracing::info!(
            info_hash,
            timeout = ?config.torrent_validation_timeout,
            "Validating torrent metadata"
        );
        let result = tokio::time::timeout(config.torrent_validation_timeout, init_fut).await;

        match result {
            Ok(Ok(())) => {
                let name = managed.name().unwrap_or_else(|| "unknown".into());
                let stats = managed.stats();
                self.span.in_scope(|| {
                    tracing::info!(
                        name,
                        info_hash,
                        total = %format_bytes(stats.total_bytes),
                        "Torrent metadata validated"
                    )
                });
            }
            Ok(Err(e)) => {
                return Err(crate::app::Error::Generic(format!(
                    "Torrent init failed: {e}"
                )));
            }
            Err(_) => {
                let stats = managed.stats();
                let peers = stats
                    .live
                    .as_ref()
                    .map(|l| l.snapshot.peer_stats.queued + l.snapshot.peer_stats.live)
                    .unwrap_or(0);
                self.span.in_scope(|| {
                    tracing::warn!(info_hash, peers, timeout = ?config.torrent_validation_timeout, "Torrent metadata timeout");
                });
                return Err(crate::app::Error::Generic(format!(
                    "Timed out waiting for torrent metadata ({peers} peers found but metadata exchange incomplete)"
                )));
            }
        }

        Ok(super::TorrentHandle { managed })
    }

    /// Mark a file as selected for download on an already-present torrent.
    /// Merges with any files already selected; does not replace.
    /// Also registers a persistent FileStream so librqbit prioritises
    /// sequential pieces from the start of the file (32MB lookahead).
    pub(super) async fn select_file(&self, key: &EngineKey) -> crate::app::Result<()> {
        let id = TorrentIdOrHash::parse(&key.info_hash)
            .map_err(|e| crate::app::Error::Generic(format!("Invalid info hash: {e}")))?;
        let handle = self
            .session
            .get(id)
            .ok_or_else(|| crate::app::Error::Generic("Torrent not in session".into()))?;

        if handle.is_paused() {
            self.session
                .unpause(&handle)
                .await
                .map_err(|e| crate::app::Error::Generic(format!("Failed to unpause: {e}")))?;
        }

        let mut files: std::collections::HashSet<usize> = handle
            .only_files()
            .unwrap_or_default()
            .into_iter()
            .collect();
        if files.insert(key.file_idx) {
            self.session
                .update_only_files(&handle, &files)
                .await
                .map_err(|e| {
                    crate::app::Error::Generic(format!("Failed to update file selection: {e}"))
                })?;
        }

        let mut handles = self.stream_handles.lock().await;
        if !handles.contains_key(key)
            && let Ok(reader) = self.stream(&key.info_hash, key.file_idx)
        {
            handles.insert(key.clone(), reader);
        }
        Ok(())
    }

    /// Stop a file from the selected torrent and release its prioritization
    /// stream. The torrent itself is left in the session unless it is the last file,
    /// in which case it is removed. Files are kept on disk.
    pub(super) async fn stop(&self, key: &EngineKey) -> crate::app::Result<()> {
        let id = TorrentIdOrHash::parse(&key.info_hash)
            .map_err(|e| crate::app::Error::Generic(format!("Invalid info hash: {e}")))?;
        let Some(handle) = self.session.get(id) else {
            return Ok(());
        };

        // First deselect the file for the torrent
        let files = self.deselect_file(handle, key.file_idx).await?;

        self.stream_handles.lock().await.remove(key);

        // If the torrent is empty, stop it
        if files.is_empty() {
            self.stop_torrent(&key.info_hash, false).await?;
        }

        Ok(())
    }

    /// Remove a whole torrent and delete its downloaded files.
    pub(super) async fn stop_and_delete(&self, key: &EngineKey) -> crate::app::Result<()> {
        let id = TorrentIdOrHash::parse(&key.info_hash)
            .map_err(|e| crate::app::Error::Generic(format!("Invalid info hash: {e}")))?;
        let Some(handle) = self.session.get(id) else {
            self.remove_file(&key.info_hash, key.file_idx).await?;
            return Ok(());
        };

        // First deselect the file for the torrent
        let files = self.deselect_file(handle, key.file_idx).await?;

        self.stream_handles.lock().await.remove(key);

        self.remove_file(&key.info_hash, key.file_idx).await?;

        // If the torrent is empty, stop it and delete the whole folder
        if files.is_empty() {
            self.stop_torrent(&key.info_hash, true).await?;
        }

        Ok(())
    }

    async fn deselect_file(
        &self,
        handle: Arc<librqbit::ManagedTorrent>,
        file_idx: usize,
    ) -> crate::app::Result<std::collections::HashSet<usize>> {
        let mut files: std::collections::HashSet<usize> = handle
            .only_files()
            .unwrap_or_default()
            .into_iter()
            .collect();
        if files.remove(&file_idx) {
            self.session
                .update_only_files(&handle, &files)
                .await
                .map_err(|e| {
                    crate::app::Error::Generic(format!("Failed to update file selection: {e}"))
                })?;
        }

        Ok(files)
    }

    async fn remove_file(&self, info_hash: &str, file_idx: usize) -> crate::app::Result<()> {
        let path = self.file_path(info_hash, file_idx)?;
        if let Err(err) = tokio::fs::remove_file(path).await
            && err.kind() != std::io::ErrorKind::NotFound
        {
            return Err(err.into());
        }

        Ok(())
    }

    /// Remove a whole torrent from the session.
    async fn stop_torrent(&self, info_hash: &str, delete_files: bool) -> crate::Result<()> {
        self.drop_stream_handles(info_hash).await;
        if let Ok(id) = TorrentIdOrHash::parse(info_hash) {
            let name = self.session.get(id).and_then(|h| h.name());
            self.session.delete(id, delete_files).await.map_err(|err| {
                crate::app::Error::Generic(format!("Could not stop the torrent: {err}"))
            })?;
            self.span.in_scope(|| {
                tracing::info!(
                    info_hash,
                    name,
                    "Torrent stopped (files {})",
                    if delete_files { "deleted" } else { "kept" }
                )
            });
        }

        Ok(())
    }

    async fn drop_stream_handles(&self, info_hash: &str) {
        let mut handles = self.stream_handles.lock().await;
        handles.retain(
            |EngineKey {
                 info_hash: hash, ..
             },
             _| info_hash != hash.as_str(),
        );
    }

    /// Pause a single torrent. Because librqbit doesn't support pausing individual files within a
    /// torrent, this method deselects the specified file if it is not the last one - in that case
    /// the torrent itself is paused.
    ///
    /// The only differences with [`Self::stop`] are that the this method does not stop the torrent itself
    /// and does not deselect the last file.
    pub(super) async fn pause(&self, key: &EngineKey) -> crate::app::Result<()> {
        let id = TorrentIdOrHash::parse(&key.info_hash)
            .map_err(|e| crate::app::Error::Generic(format!("Invalid info hash: {e}")))?;
        let Some(handle) = self.session.get(id) else {
            return Ok(());
        };

        if handle.only_files().unwrap_or_default().len() > 1 {
            self.stop(key).await?;
        } else if !handle.is_paused() {
            self.session
                .pause(&handle)
                .await
                .map_err(|e| crate::app::Error::Generic(format!("Failed to pause: {e}")))?;
        }

        Ok(())
    }

    /// Get a streaming reader for a torrent file via the Api.
    /// The reader blocks on missing pieces and prioritizes sequential download.
    pub(super) fn stream(
        &self,
        info_hash: &str,
        file_idx: usize,
    ) -> crate::app::Result<TorrentFileReader> {
        let id = TorrentIdOrHash::parse(info_hash)
            .map_err(|e| crate::app::Error::Generic(format!("Invalid info hash: {e}")))?;

        let file_stream = self
            .api
            .api_stream(id, file_idx)
            .map_err(|e| crate::app::Error::Generic(format!("Failed to create stream: {e}")))?;

        let len = file_stream.len();
        Ok(TorrentFileReader {
            inner: Box::pin(file_stream),
            len,
        })
    }

    /// Get the on-disk file path for a torrent file.
    pub fn file_path(
        &self,
        info_hash: &str,
        file_idx: usize,
    ) -> crate::app::Result<std::path::PathBuf> {
        let id = TorrentIdOrHash::parse(info_hash)
            .map_err(|e| crate::app::Error::Generic(format!("Invalid info hash: {e}")))?;

        let details = self.api.api_torrent_details(id).map_err(|e| {
            crate::app::Error::Generic(format!("Failed to get torrent details: {e}"))
        })?;

        let files = details
            .files
            .ok_or_else(|| crate::app::Error::Generic("No file metadata available".into()))?;

        let file = files.get(file_idx).ok_or_else(|| {
            crate::app::Error::Generic(format!("File index {file_idx} not found"))
        })?;

        let mut path = std::path::PathBuf::from(&details.output_folder);
        for component in &file.components {
            path.push(component);
        }
        Ok(path)
    }

    /// Get the number of audio tracks in a file using ffprobe. `input` is an
    /// ffprobe input: either an on-disk path or, for a still-downloading
    /// torrent, the local HTTP stream URL so ffprobe reads through the blocking,
    /// range-capable reader (the same one the transcode uses) instead of the
    /// sparse on-disk file.
    pub async fn audio_tracks(input: impl AsRef<std::ffi::OsStr>) -> Vec<super::AudioTrack> {
        let output = tokio::process::Command::new("ffprobe")
            .args([
                "-v",
                "quiet",
                "-print_format",
                "json",
                "-show_streams",
                "-select_streams",
                "a",
            ])
            .arg(input)
            .output();
        let output = match Self::ffprobe_output(output).await {
            Some(o) => o,
            None => return vec![],
        };

        #[derive(serde::Deserialize)]
        struct Probe {
            streams: Vec<ProbeStream>,
        }
        #[derive(serde::Deserialize)]
        struct ProbeStream {
            index: usize,
            codec_name: Option<String>,
            channels: Option<u32>,
            tags: Option<ProbeTags>,
        }
        #[derive(serde::Deserialize)]
        struct ProbeTags {
            language: Option<String>,
            title: Option<String>,
        }

        let probe: Probe = match serde_json::from_slice(&output) {
            Ok(p) => p,
            Err(_) => return vec![],
        };

        probe
            .streams
            .into_iter()
            .enumerate()
            .map(|(i, s)| {
                let tags = s.tags.as_ref();
                let lang = tags.and_then(|t| t.language.clone());
                let title = tags.and_then(|t| t.title.clone());
                let codec = s.codec_name.unwrap_or_default();
                let name = title.unwrap_or_else(|| {
                    let codec_upper = codec.to_uppercase();
                    let ch = s
                        .channels
                        .map(|c| match c {
                            1 => "Mono",
                            2 => "Stereo",
                            6 => "5.1",
                            8 => "7.1",
                            _ => "",
                        })
                        .unwrap_or("");
                    format!("{codec_upper} {ch}").trim().to_string()
                });
                super::AudioTrack {
                    index: s.index,
                    stream_index: i,
                    name,
                    language: lang,
                    codec: codec.to_lowercase(),
                }
            })
            .collect()
    }

    /// Get the duration of a media file in seconds. `input` is an ffprobe input
    /// (on-disk path or local HTTP stream URL).
    pub async fn probe_duration(input: impl AsRef<std::ffi::OsStr>) -> Option<f64> {
        let output = tokio::process::Command::new("ffprobe")
            .args(["-v", "quiet", "-print_format", "json", "-show_format"])
            .arg(input)
            .output();
        let stdout = Self::ffprobe_output(output).await?;

        #[derive(serde::Deserialize)]
        struct Probe {
            format: ProbeFormat,
        }
        #[derive(serde::Deserialize)]
        struct ProbeFormat {
            duration: Option<String>,
        }

        let probe: Probe = serde_json::from_slice(&stdout).ok()?;
        probe.format.duration.and_then(|d| d.parse::<f64>().ok())
    }

    /// Get embedded chapters of a media file. `input` is an ffprobe input
    /// (on-disk path or local HTTP stream URL). Chapters without a title get a
    /// generated "Chapter N" name; an unchaptered file returns an empty vec.
    pub async fn chapters(input: &str) -> Vec<super::Chapter> {
        let output = tokio::process::Command::new("ffprobe")
            .args(["-v", "quiet", "-print_format", "json", "-show_chapters"])
            .arg(input)
            .output();
        let output = match Self::ffprobe_output(output).await {
            Some(o) => o,
            None => return vec![],
        };

        #[derive(serde::Deserialize)]
        struct Probe {
            chapters: Vec<ProbeChapter>,
        }
        #[derive(serde::Deserialize)]
        struct ProbeChapter {
            start_time: Option<String>,
            end_time: Option<String>,
            tags: Option<ProbeTags>,
        }
        #[derive(serde::Deserialize)]
        struct ProbeTags {
            title: Option<String>,
        }

        let probe: Probe = match serde_json::from_slice(&output) {
            Ok(p) => p,
            Err(_) => return vec![],
        };

        probe
            .chapters
            .into_iter()
            .enumerate()
            .filter_map(|(i, c)| {
                let start = c.start_time.and_then(|s| s.parse::<f64>().ok())?;
                let end = c.end_time.and_then(|s| s.parse::<f64>().ok())?;
                let title = c
                    .tags
                    .and_then(|t| t.title)
                    .filter(|t| !t.trim().is_empty())
                    .unwrap_or_else(|| format!("Chapter {}", i + 1));
                Some(super::Chapter { start, end, title })
            })
            .collect()
    }

    /// Await an ffprobe `.output()` future with a timeout, returning its stdout
    /// only on a clean exit. Probing over the torrent HTTP stream can block on
    /// missing pieces, so the timeout prevents a wedged probe from hanging.
    async fn ffprobe_output(
        fut: impl std::future::Future<Output = std::io::Result<std::process::Output>>,
    ) -> Option<Vec<u8>> {
        match tokio::time::timeout(std::time::Duration::from_secs(30), fut).await {
            Ok(Ok(o)) if o.status.success() => Some(o.stdout),
            _ => None,
        }
    }

    /// Get embedded subtitle tracks in a file using ffprobe. `input` is an
    /// ffprobe input (on-disk path or local HTTP stream URL).
    pub async fn subtitle_tracks(
        input: impl AsRef<std::ffi::OsStr>,
    ) -> Vec<super::EmbeddedSubtitleTrack> {
        let output = tokio::process::Command::new("ffprobe")
            .args([
                "-v",
                "quiet",
                "-print_format",
                "json",
                "-show_streams",
                "-select_streams",
                "s",
            ])
            .arg(input)
            .output();
        let output = match Self::ffprobe_output(output).await {
            Some(o) => o,
            None => return vec![],
        };

        #[derive(serde::Deserialize)]
        struct Probe {
            streams: Vec<ProbeStream>,
        }
        #[derive(serde::Deserialize)]
        struct ProbeStream {
            index: usize,
            codec_name: Option<String>,
            tags: Option<ProbeTags>,
        }
        #[derive(serde::Deserialize)]
        struct ProbeTags {
            language: Option<String>,
            title: Option<String>,
        }

        let probe: Probe = match serde_json::from_slice(&output) {
            Ok(p) => p,
            Err(_) => return vec![],
        };

        probe
            .streams
            .into_iter()
            .filter(|s| {
                s.codec_name
                    .as_deref()
                    .map(|c| super::TEXT_SUB_CODECS.contains(&c))
                    .unwrap_or(false)
            })
            .enumerate()
            .map(|(i, s)| {
                let tags = s.tags.as_ref();
                let lang = tags.and_then(|t| t.language.clone());
                let title = tags.and_then(|t| t.title.clone());
                let codec = s.codec_name.unwrap_or_default();
                let name = title.unwrap_or_else(|| {
                    let mut label = codec.to_uppercase();
                    if let Some(ref l) = lang {
                        label = format!("{l} ({label})");
                    }
                    label
                });
                super::EmbeddedSubtitleTrack {
                    index: s.index,
                    stream_index: i,
                    language: lang,
                    name,
                    codec,
                }
            })
            .collect()
    }

    /// Extract subtitle cues from an embedded subtitle track.
    /// Runs ffmpeg with a timeout since the file may be partially downloaded.
    pub async fn extract_subtitle_cues(
        input: &str,
        stream_index: usize,
    ) -> Vec<crate::subtitles::SubtitleCue> {
        let mut child = match tokio::process::Command::new("ffmpeg")
            .args([
                "-i",
                input,
                "-map",
                &format!("0:s:{stream_index}"),
                "-f",
                "srt",
                "pipe:1",
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(_) => return vec![],
        };

        let stdout = match child.stdout.take() {
            Some(s) => s,
            None => return vec![],
        };

        use tokio::io::AsyncReadExt;
        let result = tokio::time::timeout(std::time::Duration::from_secs(15), async {
            let mut buf = Vec::new();
            let mut reader = tokio::io::BufReader::new(stdout);
            reader.read_to_end(&mut buf).await.ok();
            buf
        })
        .await;

        let _ = child.kill().await;

        let output = match result {
            Ok(buf) => buf,
            Err(_) => return vec![],
        };

        let text = String::from_utf8_lossy(&output);
        crate::subtitles::parse_srt(&text)
    }

    /// Gracefully shut down the torrent engine.
    pub async fn shutdown(&self) {
        self.span
            .in_scope(|| tracing::info!("Shutting down torrent engine"));
        self.cancel.cancel();
    }

    /// Returns the info hash of every currently-managed torrent. Used by the
    /// stats broadcaster to fan a periodic `streams_stats` event out to WS
    /// subscribers.
    pub fn active_info_hashes(&self) -> Vec<String> {
        self.session
            .with_torrents(|iter| iter.map(|(_, h)| h.info_hash().as_string()).collect())
    }

    /// `(info_hash, file_idx)` for every file currently being streamed via
    /// `stream()`. Used by the pieces broadcaster to push bitmap updates only
    /// for files a client is actively watching.
    pub async fn active_streams(&self) -> Vec<EngineKey> {
        self.stream_handles.lock().await.keys().cloned().collect()
    }

    /// Get stats for an active torrent by info hash.
    pub fn stats(&self, info_hash: &str) -> crate::app::Result<librqbit::TorrentStats> {
        let id = TorrentIdOrHash::parse(info_hash)
            .map_err(|e| crate::app::Error::Generic(format!("Invalid info hash: {e}")))?;
        let handle = self
            .session
            .get(id)
            .ok_or_else(|| crate::app::Error::Generic("Torrent not found".into()))?;
        Ok(handle.stats())
    }

    /// Get a bucketed piece map for a specific file in a torrent.
    /// Returns a vector of values 0–255 representing the download ratio
    /// of each bucket (0 = no pieces, 255 = all pieces downloaded).
    pub fn piece_map(&self, key: &EngineKey, bucket_count: usize) -> crate::app::Result<Vec<u8>> {
        let id = TorrentIdOrHash::parse(&key.info_hash)
            .map_err(|e| crate::app::Error::Generic(format!("Invalid info hash: {e}")))?;

        // Get the file's piece range from torrent metadata
        let handle = self
            .session
            .get(id)
            .ok_or_else(|| crate::app::Error::Generic("Torrent not found".into()))?;

        let piece_range = handle
            .with_metadata(|meta| {
                meta.info
                    .iter_file_details_ext(&meta.lengths)
                    .ok()
                    .and_then(|mut iter| iter.nth(key.file_idx))
                    .map(|f| f.pieces)
            })
            .map_err(|e| crate::app::Error::Generic(format!("No metadata: {e}")))?
            .ok_or_else(|| {
                crate::app::Error::Generic(format!("File index {} not found", key.file_idx))
            })?;

        let dump = self
            .api
            .api_dump_haves(id)
            .map_err(|e| crate::app::Error::Generic(format!("Failed to get piece map: {e}")))?;

        // Parse the debug output of BitSlice which looks like:
        // "BitSlice<u8, Msb0> [1, 0, 1, 1, 0, ...]"
        // Find the actual bracket-delimited list.
        let bracket_start = dump.find('[').unwrap_or(0);
        let inner = dump[bracket_start..]
            .trim_start_matches('[')
            .trim_end_matches(']');
        if inner.is_empty() {
            return Ok(vec![0u8; bucket_count]);
        }

        let all_pieces: Vec<bool> = inner.split(',').map(|s| s.trim() == "1").collect();

        // Extract only the pieces belonging to this file
        let start_piece = piece_range.start as usize;
        let end_piece = (piece_range.end as usize).min(all_pieces.len());
        if start_piece >= all_pieces.len() || start_piece >= end_piece {
            return Ok(vec![0u8; bucket_count]);
        }
        let pieces = &all_pieces[start_piece..end_piece];
        let total = pieces.len();

        let buckets = bucket_count.min(total);
        let mut result = Vec::with_capacity(buckets);

        for i in 0..buckets {
            let s = i * total / buckets;
            let e = (i + 1) * total / buckets;
            let count = e - s;
            let value = std::num::NonZeroUsize::new(count).map_or(0, |count| {
                let have = pieces[s..e].iter().filter(|&&b| b).count();
                (have.saturating_mul(255) / count) as u8
            });
            result.push(value);
        }

        Ok(result)
    }
}

fn format_bytes(bytes: u64) -> String {
    const GB: u64 = 1_073_741_824;
    const MB: u64 = 1_048_576;
    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else {
        format!("{:.0} MB", bytes as f64 / MB as f64)
    }
}
