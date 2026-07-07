use std::pin::Pin;

use std::path::Path;
use std::task::{Context, Poll};

use librqbit::ManagedTorrent;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncSeek, ReadBuf};

mod engine;
mod manager;
mod media_source;
mod supervisor;
pub mod types;

pub use engine::{AudioTrack, Chapter, EmbeddedSubtitleTrack, TorrentEngine};
pub use manager::*;
pub use media_source::{FfmpegInputSpec, MediaSource};
pub use supervisor::DownloadProgress;

/// Trait combining AsyncRead + AsyncSeek for torrent file streaming.
trait AsyncReadSeek: AsyncRead + AsyncSeek + Send + Unpin {}
impl<T: AsyncRead + AsyncSeek + Send + Unpin> AsyncReadSeek for T {}

/// A type-erased async reader for streaming media files. Wraps either
/// librqbit's `FileStream` (blocks on missing pieces) or an ordinary
/// `tokio::fs::File` for completed downloads served straight from disk.
pub struct TorrentFileReader {
    inner: Pin<Box<dyn AsyncReadSeek>>,
    pub len: u64,
}

impl TorrentFileReader {
    /// Open a completed file from disk as a reader. Used for downloads that
    /// have finished where no engine is needed.
    pub async fn open_disk(path: &Path) -> std::io::Result<Self> {
        let file = tokio::fs::File::open(path).await?;
        let len = file.metadata().await?.len();
        Ok(Self {
            inner: Box::pin(file),
            len,
        })
    }
}

impl AsyncRead for TorrentFileReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        self.inner.as_mut().poll_read(cx, buf)
    }
}

impl AsyncSeek for TorrentFileReader {
    fn start_seek(mut self: Pin<&mut Self>, position: std::io::SeekFrom) -> std::io::Result<()> {
        self.inner.as_mut().start_seek(position)
    }

    fn poll_complete(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<u64>> {
        self.inner.as_mut().poll_complete(cx)
    }
}

/// Text-based subtitle codecs that can be extracted as SRT
const TEXT_SUB_CODECS: &[&str] = &["srt", "subrip", "ass", "ssa", "webvtt", "mov_text"];

/// Well-known public trackers as fallback for magnet links.
const PUBLIC_TRACKERS: &[&str] = &[
    "udp://tracker.opentrackr.org:1337/announce",
    "udp://open.stealth.si:80/announce",
    "udp://tracker.torrent.eu.org:451/announce",
    "udp://open.demonii.com:1337/announce",
    "udp://explodie.org:6969/announce",
    "udp://tracker.tiny-vps.com:6969/announce",
    "udp://tracker.moeking.me:6969/announce",
    "udp://tracker1.bt.moack.co.kr:80/announce",
    "udp://tracker.theoks.net:6969/announce",
    "udp://tracker.bittor.pw:1337/announce",
    "udp://p4p.arenabg.com:1337/announce",
    "http://tracker.files.fm:6969/announce",
    "udp://tracker.dler.org:6969/announce",
];

/// Torrent cache services that serve .torrent files by info hash.
/// Tried in order; first successful response wins.
const TORRENT_CACHES: &[&str] = &[
    "https://itorrents.org/torrent/{}.torrent",
    "https://torrage.info/torrent/{}.torrent",
];

pub struct TorrentHandle {
    pub managed: Arc<ManagedTorrent>,
}

impl TorrentHandle {
    /// Get download progress: (downloaded_bytes, total_bytes)
    pub fn progress(&self) -> (u64, u64) {
        let stats = self.managed.stats();
        (stats.progress_bytes, stats.total_bytes)
    }
}
