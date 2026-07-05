//! HTTP endpoints that don't fit the JSON RPC pattern — range-served video
//! bytes, HLS segment files, image/asset proxies. These mount as plain Axum
//! routes alongside `rpc_router()`.

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::http::header;
use axum::response::IntoResponse;
use axum::routing::get;

use crate::app::{AppContext, Error};
use crate::hls;
use crate::torrent::TorrentEngine;

// Paths come from the `#[draad::raw]` schema (`crate::api::urls`) so the route
// strings and the frontend's `api.urls.*` builders can't drift. draad owns the
// URL contract; the byte-serving handlers below stay hand-written.
pub fn router() -> Router<AppContext> {
    Router::new()
        .route(crate::urls::STREAM, get(stream_file))
        .route(crate::urls::HLS, get(hls_serve))
        .route(crate::urls::IMAGE, get(image_proxy))
        .route(crate::urls::FILE, get(serve_file))
        .route(crate::urls::TRAILER, get(serve_trailer))
}

#[derive(serde::Deserialize)]
struct TrailerQuery {
    /// Title and release year, used to look the trailer up via the trailers-api
    /// fallback when YouTube resolution fails.
    title: Option<String>,
    year: Option<String>,
}

async fn serve_trailer(
    State(ctx): State<AppContext>,
    Path(key): Path<String>,
    Query(query): Query<TrailerQuery>,
    req: axum::http::Request<axum::body::Body>,
) -> Result<axum::response::Response, RawError> {
    // Download via yt-dlp on the first request (no-op once cached), then range-serve
    // the file so seeking works. yt-dlp does the fetching so client-bound YouTube
    // CDN URLs — which 403 when handed straight to ffmpeg — resolve correctly.
    let path = crate::trailer::ensure_cached(
        &ctx.storage,
        &key,
        query.title.as_deref(),
        query.year.as_deref(),
    )
    .await?;

    let metadata = tokio::fs::metadata(&path)
        .await
        .map_err(|_| Error::NotFound("trailer not found".into()))?;
    let file = tokio::fs::File::open(&path)
        .await
        .map_err(|_| Error::Generic("failed to open trailer".into()))?;

    let range_header = req
        .headers()
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let mut response =
        serve_range_response(file, metadata.len(), range_header.as_deref(), "video/mp4")?;
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("public, max-age=86400"),
    );
    Ok(response)
}

async fn image_proxy(
    State(ctx): State<AppContext>,
    Path(path): Path<String>,
) -> Result<axum::response::Response, RawError> {
    let path = if path.starts_with("w") || path.starts_with("original") {
        path
    } else {
        format!("original/{path}")
    };

    let cache_path = ctx.storage.join("cache/images").join(&path);
    if cache_path.exists() {
        let bytes = tokio::fs::read(&cache_path).await.map_err(Error::from)?;
        let content_type = mime_guess::from_path(&cache_path)
            .first_or_octet_stream()
            .to_string();
        return Ok((
            [
                (header::CONTENT_TYPE, content_type),
                (header::CACHE_CONTROL, "public, max-age=604800".into()),
            ],
            bytes,
        )
            .into_response());
    }

    let url = format!("https://image.tmdb.org/t/p/{path}");
    let res = ctx
        .http
        .get(&url)
        .send()
        .await
        .map_err(Error::from)?
        .error_for_status()
        .map_err(|e| Error::Generic(e.to_string()))?;

    let content_type = res
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("image/jpeg")
        .to_string();
    let bytes = res.bytes().await.map_err(Error::from)?;

    if let Some(parent) = cache_path.parent() {
        if let Err(e) = tokio::fs::create_dir_all(parent).await {
            tracing::warn!("failed to create image cache dir {}: {e}", parent.display());
        }
    }
    if let Err(e) = tokio::fs::write(&cache_path, &bytes).await {
        tracing::warn!("failed to write image cache {}: {e}", cache_path.display());
    }

    Ok((
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "public, max-age=604800".into()),
        ],
        bytes,
    )
        .into_response())
}

async fn stream_file(
    State(ctx): State<AppContext>,
    Path((info_hash, file_idx)): Path<(String, usize)>,
    req: axum::http::Request<axum::body::Body>,
) -> Result<axum::response::Response, RawError> {
    let engine = TorrentEngine::get();
    engine.start(&info_hash, file_idx, &ctx.config).await?;
    let reader = engine.stream(&info_hash, file_idx)?;
    let total_size = reader.len;

    let range_header = req
        .headers()
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    serve_range_response(reader, total_size, range_header.as_deref(), "video/mp4")
}

async fn hls_serve(
    Path((session_id, file)): Path<(String, String)>,
) -> Result<axum::response::Response, RawError> {
    if file.contains("..") || file.contains('/') {
        return Err(Error::Generic("Invalid path".into()).into());
    }

    let dir = hls::session_dir(&session_id)
        .await
        .ok_or_else(|| Error::NotFound("HLS session not found".into()))?;
    hls::touch(&session_id).await;

    let full_path = dir.join(&file);
    let bytes = match tokio::fs::read(&full_path).await {
        Ok(b) => b,
        Err(_) => {
            if let Some(error) = hls::session_error(&session_id).await {
                return Err(
                    Error::Generic(format!("Stream failed (ffmpeg exited): {error}")).into(),
                );
            }
            return Err(Error::NotFound(format!("HLS file not found: {file}")).into());
        }
    };

    let (content_type, cache) = if file.ends_with(".m3u8") {
        ("application/vnd.apple.mpegurl", "no-cache")
    } else {
        ("video/mp2t", "public, max-age=3600")
    };

    Ok(axum::http::Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CACHE_CONTROL, cache)
        .body(axum::body::Body::from(bytes))
        .unwrap())
}

async fn serve_file(
    State(ctx): State<AppContext>,
    Path(path): Path<String>,
    req: axum::http::Request<axum::body::Body>,
) -> Result<axum::response::Response, RawError> {
    if path.contains("..") {
        return Err(Error::Generic("Invalid path".into()).into());
    }
    let full_path = ctx.storage.join(&path);

    let metadata = tokio::fs::metadata(&full_path)
        .await
        .map_err(|_| Error::Generic("File not found".into()))?;
    let file = tokio::fs::File::open(&full_path)
        .await
        .map_err(|_| Error::Generic("Failed to open file".into()))?;

    let total_size = metadata.len();
    let content_type = if path.ends_with(".mp4") {
        "video/mp4"
    } else if path.ends_with(".mkv") {
        "video/x-matroska"
    } else {
        "application/octet-stream"
    };

    let range_header = req
        .headers()
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    serve_range_response(file, total_size, range_header.as_deref(), content_type)
}

fn serve_range_response<R: tokio::io::AsyncRead + tokio::io::AsyncSeek + Send + Unpin + 'static>(
    reader: R,
    total_size: u64,
    range_header: Option<&str>,
    content_type: &str,
) -> Result<axum::response::Response, RawError> {
    use axum::body::Body;
    use axum::http::Response;

    if total_size == 0 {
        return Ok(Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_LENGTH, 0)
            .body(Body::empty())
            .unwrap());
    }

    let (start, end) = if let Some(range) = range_header {
        let range = range.trim_start_matches("bytes=");
        let parts: Vec<&str> = range.splitn(2, '-').collect();
        let start: u64 = parts[0].parse().unwrap_or(0).min(total_size - 1);
        let end: u64 = if parts.len() > 1 && !parts[1].is_empty() {
            parts[1].parse().unwrap_or(total_size - 1)
        } else {
            total_size - 1
        };
        (start, end.min(total_size - 1))
    } else {
        (0, total_size - 1)
    };

    let content_length = end.saturating_sub(start) + 1;

    let body = Body::from_stream(async_stream::stream! {
        use tokio::io::{AsyncReadExt, AsyncSeekExt};
        let mut reader = std::pin::pin!(reader);
        if start > 0 && let Err(e) = reader.as_mut().seek(std::io::SeekFrom::Start(start)).await {
                yield Err(e);
                return;
        }
        let mut remaining = content_length;
        let mut buf = vec![0u8; 64 * 1024];
        while remaining > 0 {
            let to_read = (buf.len() as u64).min(remaining) as usize;
            match reader.as_mut().read(&mut buf[..to_read]).await {
                Ok(0) => break,
                Ok(n) => {
                    remaining -= n as u64;
                    yield Ok::<_, std::io::Error>(bytes::Bytes::copy_from_slice(&buf[..n]));
                }
                Err(e) => {
                    yield Err(e);
                    break;
                }
            }
        }
    });

    if range_header.is_some() {
        Ok(Response::builder()
            .status(StatusCode::PARTIAL_CONTENT)
            .header(header::CONTENT_TYPE, content_type)
            .header(header::CONTENT_LENGTH, content_length)
            .header(
                header::CONTENT_RANGE,
                format!("bytes {start}-{end}/{total_size}"),
            )
            .header(header::ACCEPT_RANGES, "bytes")
            .body(body)
            .unwrap())
    } else {
        Ok(Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, content_type)
            .header(header::CONTENT_LENGTH, total_size)
            .header(header::ACCEPT_RANGES, "bytes")
            .body(body)
            .unwrap())
    }
}

struct RawError(Error);

impl IntoResponse for RawError {
    fn into_response(self) -> axum::response::Response {
        let (status, message) = match self.0 {
            Error::NotFound(msg) => (StatusCode::NOT_FOUND, msg),
            err => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
        };
        (status, message).into_response()
    }
}

impl<E: Into<Error>> From<E> for RawError {
    fn from(e: E) -> Self {
        RawError(e.into())
    }
}
