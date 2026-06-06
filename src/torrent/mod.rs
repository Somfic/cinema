use std::pin::Pin;

use std::task::{Context, Poll};

use librqbit::ManagedTorrent;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncSeek, ReadBuf};

mod engine;

pub use engine::TorrentEngine;

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

/// Trait combining AsyncRead + AsyncSeek for torrent file streaming.
trait AsyncReadSeek: AsyncRead + AsyncSeek + Send + Unpin {}
impl<T: AsyncRead + AsyncSeek + Send + Unpin> AsyncReadSeek for T {}

/// A type-erased async reader for streaming torrent files.
/// Wraps librqbit's FileStream (which can't be named outside the crate).
pub struct TorrentFileReader {
    inner: Pin<Box<dyn AsyncReadSeek>>,
    pub len: u64,
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
