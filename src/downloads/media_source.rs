//! Where the bytes of a downloaded file live right now.
//!
//! Two backing stores exist: the torrent engine (blocks on missing pieces
//! while a download is in flight) and the on-disk file (a completed download
//! is just a file). This type unifies both behind a single interface so
//! consumers - HTTP range serving, ffmpeg-fed transcodes, codec probes - can
//! stay agnostic about which one they're reading from.
//!
//! Produced by [`MediaSource::ensure_and_locate`], which
//! guarantees that for the `Engine` variant the torrent is loaded and the file
//! is selected; and for the `Disk` variant that the file exists on disk.

use std::path::{Path, PathBuf};

use tokio::io::AsyncWriteExt;
use tokio::process::ChildStdin;

use super::{TorrentEngine, TorrentFileReader};
use crate::app::Result;

/// Where the bytes of a `(info_hash, file_idx)` live right now.
pub enum MediaSource {
    /// Download is complete; the file is fully on disk.
    Disk { path: PathBuf },
    /// Download is in progress. The engine has the torrent loaded and the
    /// file selected. `sparse_path` points to the on-disk file backing the
    /// torrent - safe to hand to ffprobe (which is happy with holes), but
    /// reads that need coherent bytes must go through [`open_reader`], which
    /// blocks on missing pieces.
    Engine {
        info_hash: String,
        file_idx: usize,
        sparse_path: PathBuf,
    },
}

/// How to hand this source to ffmpeg's `-i`.
pub enum FfmpegInputSpec<'a> {
    /// `-i <path>`: the file is on disk and ffmpeg reads it directly.
    Path(&'a Path),
    /// `-i pipe:0`: caller must configure `stdin(Stdio::piped())` and drive
    /// [`MediaSource::spawn_stdin_pump`] on the resulting `ChildStdin`.
    Pipe,
}

impl MediaSource {
    /// Ensure the download is progressing (or complete) and return a
    /// [`crate::downloads::MediaSource`] pointing at where its bytes live.
    ///
    /// - Completed row with a persisted `output_path`: returns `Disk`.
    /// - Otherwise: returns `Engine`. The torrent is guaranteed loaded and
    ///   the file selected because `ensure_download` has just run.
    pub async fn ensure_and_locate(
        download_manager: &crate::downloads::Handle,
        storage: &crate::app::Storage,
        info_hash: &str,
        file_idx: i32,
        priority: super::DownloadPriority,
    ) -> crate::app::Result<Self> {
        let (_, outcome) = download_manager
            .ensure_download(info_hash, file_idx, priority)
            .await?;

        if let super::StartOutcome::AlreadyComplete { output_path } = outcome
            && let Some(path) = output_path.as_deref()
        {
            let path = storage.join(path);
            return Ok(crate::downloads::MediaSource::Disk { path });
        }

        // Non-completed: manager.start (inside ensure_download) has loaded the
        // torrent and selected the file, so engine.file_path is safe.
        let engine = crate::downloads::TorrentEngine::get();
        let sparse_path = engine.file_path(info_hash, file_idx as usize)?;
        Ok(crate::downloads::MediaSource::Engine {
            info_hash: info_hash.to_string(),
            file_idx: file_idx as usize,
            sparse_path,
        })
    }

    /// A path safe to hand to ffprobe. Works in both modes because the on-disk
    /// file exists in `Engine` mode too - it's just sparse.
    pub fn probe_path(&self) -> &Path {
        match self {
            Self::Disk { path } => path,
            Self::Engine { sparse_path, .. } => sparse_path,
        }
    }

    /// A coherent source specifier for ffmpeg-family probes that need to seek
    /// or read past a torrent's missing pieces. For `Disk`, this is the on-disk
    /// path; for `Engine`, a loopback HTTP URL through the blocking stream
    /// reader (the raw sparse file would trip up e.g. a moov-atom seek).
    pub fn coherent_input(&self, config: &crate::config::Config) -> String {
        match self {
            Self::Disk { path, .. } => path.to_string_lossy().into_owned(),
            Self::Engine {
                info_hash,
                file_idx,
                ..
            } => format!(
                "http://127.0.0.1:{}/api/stream/{}/{}",
                config.port, info_hash, file_idx
            ),
        }
    }

    /// How to attach this source as ffmpeg's input.
    pub fn ffmpeg_input_spec(&self) -> FfmpegInputSpec<'_> {
        match self {
            Self::Disk { path, .. } => FfmpegInputSpec::Path(path),
            Self::Engine { .. } => FfmpegInputSpec::Pipe,
        }
    }

    /// Open a reader over the source. `Disk` opens the file directly; `Engine`
    /// returns the blocking-on-missing-pieces librqbit stream.
    pub async fn open_reader(&self) -> Result<TorrentFileReader> {
        match self {
            Self::Disk { path } => TorrentFileReader::open_disk(path)
                .await
                .map_err(crate::app::CinemaError::IoError),
            Self::Engine {
                info_hash,
                file_idx,
                ..
            } => TorrentEngine::get().stream(info_hash, *file_idx),
        }
    }

    /// Spawn a background task that copies bytes from this source into
    /// `stdin`. Intended for `Engine` sources feeding `-i pipe:0` - for `Disk`
    /// sources you don't need a pump (ffmpeg reads the file itself).
    pub async fn spawn_stdin_pump(&self, stdin: ChildStdin) -> Result<tokio::task::JoinHandle<()>> {
        let reader = self.open_reader().await?;
        let span = tracing::Span::current();
        Ok(tokio::spawn(tracing::Instrument::instrument(
            async move {
                use tokio::io::AsyncReadExt;
                let mut reader = reader;
                let mut stdin = stdin;
                let mut buf = vec![0u8; 64 * 1024];
                loop {
                    match reader.read(&mut buf).await {
                        Ok(0) => break,
                        Ok(n) => {
                            if stdin.write_all(&buf[..n]).await.is_err() {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
            },
            span,
        )))
    }
}
