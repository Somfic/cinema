//! Where the bytes of a downloaded file live right now.
//!
//! Two backing stores exist: the torrent engine (blocks on missing pieces
//! while a download is in flight) and the on-disk file (a completed download
//! is just a file). This type unifies both behind a single interface so
//! consumers - HTTP range serving, transcodes, probes - can stay agnostic
//! about which one they're reading from.
//!
//! Produced by [`MediaSource::ensure_and_locate`], which
//! guarantees that for the `Engine` variant the torrent is loaded and the file
//! is selected; and for the `Disk` variant that the file exists on disk.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::{TorrentEngine, TorrentFileReader};
use crate::app::Result;

/// Where the bytes of a `(info_hash, file_idx)` live right now.
pub enum MediaSource {
    /// Download is complete; the file is fully on disk.
    Disk { path: PathBuf },
    /// Download is in progress. The engine has the torrent loaded and the
    /// file selected. `sparse_path` points to the on-disk file backing the
    /// torrent, which has holes where pieces are missing: reads that need
    /// coherent bytes must go through [`open_reader`], which blocks on
    /// missing pieces.
    Engine {
        info_hash: String,
        file_idx: usize,
        sparse_path: PathBuf,
    },
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

    /// Where the file lives on disk. For a download in progress this is the
    /// sparse file the torrent is filling in.
    pub fn path(&self) -> &Path {
        match self {
            Self::Disk { path } => path,
            Self::Engine { sparse_path, .. } => sparse_path,
        }
    }

    /// The source as input for a media pipeline. A download in progress is
    /// read through the torrent stream, so a pipeline waits for missing
    /// pieces instead of reading holes.
    pub async fn media_input(&self) -> Result<media::Input> {
        match self {
            Self::Disk { path } => Ok(media::Input::File(path.clone())),
            Self::Engine {
                info_hash,
                file_idx,
                ..
            } => {
                let len = open_stream(info_hash, *file_idx).await?.len;
                let opener = TorrentOpener {
                    info_hash: info_hash.clone(),
                    file_idx: *file_idx,
                    len,
                };
                Ok(media::Input::stream(
                    Arc::new(opener),
                    format!("{info_hash}/{file_idx}"),
                ))
            }
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
}

/// How long a torrent may take to finish initialising (checking the pieces
/// already on disk) before a stream on it gives up.
const INITIALISING_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Opens a stream, waiting out a torrent that is still initialising: right
/// after a (re)start librqbit refuses streams until it has checked its files.
async fn open_stream(info_hash: &str, file_idx: usize) -> Result<TorrentFileReader> {
    let deadline = tokio::time::Instant::now() + INITIALISING_TIMEOUT;
    loop {
        match TorrentEngine::get().stream(info_hash, file_idx) {
            Err(err)
                if err.to_string().contains("initializing")
                    && tokio::time::Instant::now() < deadline =>
            {
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            }
            result => return result,
        }
    }
}

struct TorrentOpener {
    info_hash: String,
    file_idx: usize,
    len: u64,
}

impl media::Opener for TorrentOpener {
    fn size(&self) -> u64 {
        self.len
    }

    fn open(&self) -> futures::future::BoxFuture<'static, std::io::Result<media::BoxReader>> {
        let (info_hash, file_idx) = (self.info_hash.clone(), self.file_idx);
        Box::pin(async move {
            open_stream(&info_hash, file_idx)
                .await
                .map(|reader| Box::pin(reader) as media::BoxReader)
                .map_err(|e| std::io::Error::other(e.to_string()))
        })
    }
}
