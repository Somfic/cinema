use std::{
    collections::HashMap,
    sync::{Arc, OnceLock},
};

use librqbit::{
    AddTorrent, AddTorrentOptions, AddTorrentResponse, Api, Session, SessionOptions,
    api::TorrentIdOrHash,
};
use tokio_util::sync::CancellationToken;

use crate::{config::Config, torrent::TorrentFileReader};

static ENGINE: OnceLock<TorrentEngine> = OnceLock::new();

pub struct TorrentEngine {
    session: Arc<Session>,
    api: Api,
    http: reqwest::Client,
    cancel: CancellationToken,
    span: tracing::Span,
    /// Keep a FileStream alive per (info_hash, file_idx) to maintain
    /// sequential piece prioritization from librqbit's streaming system.
    stream_handles: tokio::sync::Mutex<HashMap<(String, usize), TorrentFileReader>>,
}

impl TorrentEngine {
    pub async fn init(
        config: &Config,
        storage: &crate::app::Storage,
        http: reqwest::Client,
    ) -> crate::app::Result<()> {
        let output_folder = storage.join("torrents");
        tokio::fs::create_dir_all(&output_folder).await?;

        let cancel = CancellationToken::new();
        let session_cancel = cancel.clone();

        let opts = SessionOptions {
            disable_dht: !config.use_dht,
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
                http,
                cancel,
                span,
                stream_handles: tokio::sync::Mutex::new(HashMap::new()),
            })
            .map_err(|_| crate::app::Error::Generic("Torrent engine already initialized".into()))?;

        tracing::info!("Torrent engine initialized");
        Ok(())
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

    /// Start a torrent for the given info_hash and file index.
    /// Idempotent -- returns immediately if already active.
    pub async fn start(
        &self,
        info_hash: &str,
        file_idx: usize,
        config: &Config,
    ) -> crate::app::Result<super::TorrentHandle> {
        // Fast path: torrent already active, no logging/fetching needed
        if let Ok(id) = TorrentIdOrHash::parse(info_hash)
            && let Some(handle) = self.session.get(id)
        {
            let mut files: std::collections::HashSet<usize> = handle
                .only_files()
                .unwrap_or_default()
                .into_iter()
                .collect();
            if !files.contains(&file_idx) {
                files.insert(file_idx);
                let _ = self.session.update_only_files(&handle, &files).await;
            }
            // Unpause this torrent and pause others
            self.pause_all_except(info_hash);

            // Ensure a persistent stream exists for piece prioritization
            let key = (info_hash.to_lowercase(), file_idx);
            let mut handles = self.stream_handles.lock().await;
            if !handles.contains_key(&key)
                && let Ok(reader) = self.stream(info_hash, file_idx)
            {
                handles.insert(key, reader);
            }
            return Ok(super::TorrentHandle { managed: handle });
        }

        let make_opts = || AddTorrentOptions {
            only_files: Some(vec![file_idx]),
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

        // Wait for metadata + initial check, but don't block forever.
        // Log progress so slow peer discovery is visible.
        {
            let init_fut = managed.wait_until_initialized();

            tracing::info!(
                info_hash,
                timeout = ?config.torrent_validation_timeout,
                "Validating torrent metadata"
            );
            let result = tokio::time::timeout(config.torrent_validation_timeout, init_fut).await;

            match result {
                Ok(Ok(())) => {
                    tracing::info!(info_hash, "Torrent metadata validated");
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
        }

        // Pause other torrents to prioritize the one being watched
        self.pause_all_except(info_hash);

        // Register a persistent FileStream so librqbit prioritizes
        // sequential pieces from the start of the file (32MB lookahead).
        {
            let key = (info_hash.to_lowercase(), file_idx);
            let mut handles = self.stream_handles.lock().await;
            if !handles.contains_key(&key)
                && let Ok(reader) = self.stream(info_hash, file_idx)
            {
                handles.insert(key, reader);
            }
        }

        let name = managed.name().unwrap_or_else(|| "unknown".into());
        let stats = managed.stats();
        self.span.in_scope(|| {
            tracing::info!(
                name,
                info_hash,
                file_idx,
                total = %format_bytes(stats.total_bytes),
                "Torrent streaming"
            )
        });

        Ok(super::TorrentHandle { managed })
    }

    /// Get a streaming reader for a torrent file via the Api.
    /// The reader blocks on missing pieces and prioritizes sequential download.
    pub fn stream(
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

    /// Get the number of audio tracks in a file using ffprobe.
    pub async fn audio_tracks(path: &std::path::Path) -> Vec<super::AudioTrack> {
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
            .arg(path)
            .output()
            .await;

        let output = match output {
            Ok(o) if o.status.success() => o.stdout,
            _ => return vec![],
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

    /// Get the duration of a media file in seconds.
    pub async fn probe_duration(path: &std::path::Path) -> Option<f64> {
        let output = tokio::process::Command::new("ffprobe")
            .args(["-v", "quiet", "-print_format", "json", "-show_format"])
            .arg(path)
            .output()
            .await
            .ok()?;

        if !output.status.success() {
            return None;
        }

        #[derive(serde::Deserialize)]
        struct Probe {
            format: ProbeFormat,
        }
        #[derive(serde::Deserialize)]
        struct ProbeFormat {
            duration: Option<String>,
        }

        let probe: Probe = serde_json::from_slice(&output.stdout).ok()?;
        probe.format.duration.and_then(|d| d.parse::<f64>().ok())
    }

    /// Get embedded subtitle tracks in a file using ffprobe.
    pub async fn subtitle_tracks(path: &std::path::Path) -> Vec<super::EmbeddedSubtitleTrack> {
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
            .arg(path)
            .output()
            .await;

        let output = match output {
            Ok(o) if o.status.success() => o.stdout,
            _ => return vec![],
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
        path: &std::path::Path,
        stream_index: usize,
    ) -> Vec<crate::subtitles::SubtitleCue> {
        let mut child = match tokio::process::Command::new("ffmpeg")
            .args([
                "-i",
                path.to_str().unwrap_or(""),
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

    /// Pause all torrents except the one with the given info hash.
    /// Also unpause the target torrent if it was paused.
    fn pause_all_except(&self, active_hash: &str) {
        let active_lower = active_hash.to_lowercase();
        self.session.with_torrents(|iter| {
            for (_, handle) in iter {
                let hash = handle.info_hash().as_string();
                if hash == active_lower {
                    if handle.is_paused() {
                        let session = self.session.clone();
                        let h = handle.clone();
                        tokio::spawn(async move {
                            let _ = session.unpause(&h).await;
                        });
                    }
                } else if !handle.is_paused() {
                    let session = self.session.clone();
                    let h = handle.clone();
                    tokio::spawn(async move {
                        let _ = session.pause(&h).await;
                    });
                }
            }
        });
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
    pub async fn active_streams(&self) -> Vec<(String, usize)> {
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
    pub fn piece_map(
        &self,
        info_hash: &str,
        file_idx: usize,
        bucket_count: usize,
    ) -> crate::app::Result<Vec<u8>> {
        let id = TorrentIdOrHash::parse(info_hash)
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
                    .and_then(|mut iter| iter.nth(file_idx))
                    .map(|f| f.pieces)
            })
            .map_err(|e| crate::app::Error::Generic(format!("No metadata: {e}")))?
            .ok_or_else(|| {
                crate::app::Error::Generic(format!("File index {file_idx} not found"))
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

    /// Remove a torrent from the session. Files are kept on disk.
    pub async fn stop(&self, info_hash: &str) {
        self.drop_stream_handles(info_hash).await;
        if let Ok(id) = TorrentIdOrHash::parse(info_hash) {
            let name = self.session.get(id).and_then(|h| h.name());
            let _ = self.session.delete(id, false).await;
            self.span
                .in_scope(|| tracing::info!(info_hash, name, "Torrent stopped (files kept)"));
        }
    }

    async fn drop_stream_handles(&self, info_hash: &str) {
        let hash = info_hash.to_lowercase();
        let mut handles = self.stream_handles.lock().await;
        handles.retain(|(h, _), _| h != &hash);
    }

    /// Remove a torrent and delete its downloaded files.
    pub async fn stop_and_delete(&self, info_hash: &str) {
        self.drop_stream_handles(info_hash).await;
        if let Ok(id) = TorrentIdOrHash::parse(info_hash) {
            let name = self.session.get(id).and_then(|h| h.name());
            let _ = self.session.delete(id, true).await;
            self.span
                .in_scope(|| tracing::info!(info_hash, name, "Torrent stopped and files deleted"));
        }
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
