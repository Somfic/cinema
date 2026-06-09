//! HTTP endpoints that don't fit the JSON RPC pattern — range-served video
//! bytes, HLS segment files, image/asset proxies. These mount as plain Axum
//! routes alongside `rpc_router()`.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::http::header;
use axum::response::IntoResponse;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::Deserialize;

use crate::app::{AppContext, Error};
use crate::hls;
use crate::torrent::TorrentEngine;

pub fn router() -> Router<AppContext> {
    Router::new()
        .route("/stream/{info_hash}/{file_idx}", get(stream_file))
        .route(
            "/stream/{info_hash}/{file_idx}/audio",
            get(stream_audio_tracks),
        )
        .route(
            "/stream/{info_hash}/{file_idx}/remux",
            post(stream_remux_hls),
        )
        .route("/hls/{session_id}/{file}", get(hls_serve))
        .route("/hls/{session_id}", delete(hls_stop_raw))
        .route("/image/{*path}", get(image_proxy))
        .route("/files/{*path}", get(serve_file))
        .route("/trailer/{key}", get(serve_trailer))
        .route("/trailer/{key}/meta", get(serve_trailer_meta))
}

async fn serve_trailer_meta(
    State(ctx): State<AppContext>,
    Path(key): Path<String>,
) -> Result<axum::response::Response, RawError> {
    let meta = crate::trailer::ensure_meta(&ctx.storage, &key).await?;
    Ok((
        StatusCode::OK,
        [(header::CACHE_CONTROL, "public, max-age=86400")],
        axum::Json(meta),
    )
        .into_response())
}

async fn serve_trailer(
    State(ctx): State<AppContext>,
    Path(key): Path<String>,
    req: axum::http::Request<axum::body::Body>,
) -> Result<axum::response::Response, RawError> {
    // Already downloaded: range-serve the cached file (supports seeking).
    if let Some(path) = crate::trailer::cached_path(&ctx.storage, &key) {
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
        return Ok(response);
    }

    // First request: stream the fragmented mp4 as ffmpeg produces it, teeing the
    // bytes into the cache so later requests hit the fast path above.
    let stream = crate::trailer::start_stream(&ctx.storage, &key).await?;
    let crate::trailer::TrailerStream {
        mut child,
        stdout,
        tmp_path,
        final_path,
    } = stream;

    let body = axum::body::Body::from_stream(async_stream::stream! {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let mut reader = stdout;
        let mut file = tokio::fs::File::create(&tmp_path).await.ok();
        let mut buf = vec![0u8; 64 * 1024];
        let mut had_error = false;

        loop {
            match reader.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    if let Some(f) = file.as_mut() {
                        let _ = f.write_all(&buf[..n]).await;
                    }
                    yield Ok::<_, std::io::Error>(bytes::Bytes::copy_from_slice(&buf[..n]));
                }
                Err(e) => {
                    had_error = true;
                    yield Err(e);
                    break;
                }
            }
        }

        // Promote the temp file to the cache only on a clean, complete run.
        let ok = !had_error && child.wait().await.map(|s| s.success()).unwrap_or(false);
        if let Some(mut f) = file.take() {
            let _ = f.flush().await;
        }
        if ok {
            let _ = tokio::fs::rename(&tmp_path, &final_path).await;
        } else {
            let _ = tokio::fs::remove_file(&tmp_path).await;
        }
    });

    Ok(axum::http::Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "video/mp4")
        .header(header::CACHE_CONTROL, "no-store")
        .body(body)
        .unwrap())
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

    if let Some(parent) = cache_path.parent()
        && let Err(e) = tokio::fs::create_dir_all(parent).await
    {
        tracing::warn!("failed to create image cache dir {}: {e}", parent.display());
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
    // Block until the torrent is loaded and its metadata is known.
    engine.ensure_torrent(&info_hash, &ctx.config).await?;
    engine.select_file(&info_hash, file_idx).await?;
    // Record the active stream in the DB so the manager picks it up too.
    // This also kicks off the per-download supervisor (progress writes,
    // async TMDB metadata resolution, etc.).
    crate::downloads::ensure_download(
        &ctx.db,
        &ctx.downloads,
        &info_hash,
        file_idx as i32,
        None,
    )
    .await?;
    let reader = engine.stream(&info_hash, file_idx)?;
    let total_size = reader.len;

    let range_header = req
        .headers()
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    serve_range_response(reader, total_size, range_header.as_deref(), "video/mp4")
}

async fn stream_audio_tracks(
    State(ctx): State<AppContext>,
    Path((info_hash, file_idx)): Path<(String, usize)>,
) -> Result<Json<serde_json::Value>, RawError> {
    let engine = TorrentEngine::get();
    let path = engine.file_path(&info_hash, file_idx)?;
    let (tracks, subtitles, duration) = tokio::join!(
        TorrentEngine::audio_tracks(&path),
        TorrentEngine::subtitle_tracks(&path),
        TorrentEngine::probe_duration(&path),
    );

    // Filter embedded subtitle tracks by configured languages
    let subtitles = {
        let allowed: Vec<&str> = ctx
            .config
            .subtitle_languages
            .iter()
            .map(|l| crate::subtitles::to_iso639_2(l))
            .collect();
        subtitles
            .into_iter()
            .filter(|s| {
                s.language
                    .as_deref()
                    .map(|l| allowed.contains(&l))
                    .unwrap_or(true) // keep tracks with unknown language
            })
            .collect::<Vec<_>>()
    };

    Ok(Json(serde_json::json!({
        "tracks": tracks,
        "subtitles": subtitles,
        "duration": duration,
    })))
}

#[derive(Deserialize)]
struct RemuxParams {
    #[serde(default)]
    audio: usize,
    #[serde(default)]
    t: f64,
    #[serde(default)]
    only_audio: bool,
}

async fn stream_remux_hls(
    State(ctx): State<AppContext>,
    Path((info_hash, file_idx)): Path<(String, usize)>,
    Query(params): Query<RemuxParams>,
) -> Result<axum::response::Response, RawError> {
    let engine = TorrentEngine::get();
    engine.ensure_torrent(&info_hash, &ctx.config).await?;
    engine.select_file(&info_hash, file_idx).await?;
    crate::downloads::ensure_download(
        &ctx.db,
        &ctx.downloads,
        &info_hash,
        file_idx as i32,
        None,
    )
    .await?;

    let (session_id, playlist_url) = hls::start_session(
        &ctx.storage,
        &ctx.config,
        hls::HlsSessionStartInput {
            info_hash,
            file_idx,
            audio_index: params.audio,
            start_time: params.t,
            only_audio: params.only_audio,
        },
    )
    .await?;

    let body = serde_json::json!({ "session_id": session_id, "playlist_url": playlist_url });
    Ok((StatusCode::OK, axum::Json(body)).into_response())
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

async fn hls_stop_raw(Path(session_id): Path<String>) -> StatusCode {
    hls::stop_session(&session_id).await;
    StatusCode::NO_CONTENT
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
