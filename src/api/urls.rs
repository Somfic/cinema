//! Browser-direct URL contracts. draad generates a typed TS URL-builder
//! (`api.urls.*`) and Rust path constants (`crate::urls::*`); `crate::raw`
//! mounts the byte-serving Axum handlers against those constants, so the path
//! is declared exactly once. draad never serves the bytes.

#[draad::raw]
pub trait Urls {
    /// Range-served video bytes for a torrent file.
    #[get("/api/stream/{info_hash}/{file_idx}")]
    fn stream(info_hash: String, file_idx: i64);

    /// Audio tracks in a torrent.
    #[get("/api/stream/{info_hash}/{file_idx}/audio")]
    fn stream_audio(info_hash: String, file_idx: i64);

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
}
