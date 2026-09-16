//! Browser-direct URL contracts. draad generates a typed TS URL-builder
//! (`api.urls.*`) and Rust path constants (`crate::urls::*`); `crate::raw`
//! mounts the byte-serving Axum handlers against those constants, so the path
//! is declared exactly once. draad never serves the bytes.

#[draad::raw]
pub trait Urls {
    /// Range-served video bytes for a torrent file.
    #[get("/api/stream/{info_hash}/{file_idx}")]
    fn stream(info_hash: String, file_idx: i64);

    /// An HLS playlist or segment for a transcode session.
    #[get("/api/hls/{session_id}/{file}")]
    fn hls(session_id: String, file: String);

    /// Cached/proxied TMDB image. `{*path}` is `{size}{tmdb_path}` (holds a
    /// slash), so it's interpolated raw rather than URL-encoded.
    #[get("/api/image/{*path}")]
    fn image(path: String);

    /// Range-served file from storage.
    #[get("/api/files/{*path}")]
    fn file(path: String);

    /// Trailer video bytes for a YouTube key.
    #[get("/api/trailer/{key}")]
    fn trailer(key: String);

    /// An external subtitle track rendered as WebVTT. `{url}` is the upstream
    /// SRT url, percent-encoded into a single path segment. Cast receivers
    /// sideload captions by URL rather than taking in-page cues, so the same
    /// tracks the inline player draws itself are also served as a file here.
    #[get("/api/subtitles/external/{url}")]
    fn external_subtitles(url: String);

    /// An embedded subtitle track of a torrent file, rendered as WebVTT.
    /// `{stream_index}` is the ffmpeg stream index (the number encoded in the
    /// frontend's `embedded:<n>` track ids).
    #[get("/api/subtitles/embedded/{info_hash}/{file_idx}/{stream_index}")]
    fn embedded_subtitles(info_hash: String, file_idx: i64, stream_index: i64);
}
