use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::http::header;
use axum::response::IntoResponse;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::app::{AppContext, Error};
use crate::downloads::Download;
use crate::streams;
use crate::streams::Stream;
use crate::subtitles::{SubtitleCue, SubtitleTrack};
use crate::tmdb::{MediaItem, MediaType, SearchResult, TmdbClient};
use crate::torrent::TorrentEngine;

pub fn router() -> OpenApiRouter<AppContext> {
    OpenApiRouter::new()
        .routes(routes!(search))
        .routes(routes!(movie_details))
        .routes(routes!(tv_details))
        .routes(routes!(movie_streams))
        .routes(routes!(tv_streams))
        .routes(routes!(start_stream))
        .routes(routes!(movie_subtitles))
        .routes(routes!(tv_subtitles))
        .routes(routes!(subtitle_cues))
        .routes(routes!(trending))
        .routes(routes!(similar))
        .routes(routes!(record_watch))
        .routes(routes!(watch_history))
        .routes(routes!(add_to_collection))
        .routes(routes!(remove_from_collection))
        .routes(routes!(get_collection))
        .routes(routes!(is_in_collection))
        .routes(routes!(list_collection_defs))
        .routes(routes!(create_collection_def))
        .routes(routes!(delete_collection_def))
        .routes(routes!(set_collection_visibility))
        .routes(routes!(reorder_collection_defs))
        .routes(routes!(reorder_collection))
        .routes(routes!(enqueue_download))
        .routes(routes!(list_downloads))
        .routes(routes!(delete_download))
        .routes(routes!(estimate_download))
        .route(
            "/stream/{info_hash}/stats",
            axum::routing::get(stream_stats),
        )
        .route(
            "/stream/{info_hash}/{file_idx}/pieces",
            axum::routing::get(stream_pieces),
        )
        .route(
            "/stream/{info_hash}/{file_idx}",
            axum::routing::get(stream_file),
        )
        .route(
            "/stream/{info_hash}/{file_idx}/audio",
            axum::routing::get(stream_audio_tracks),
        )
        .route(
            "/stream/{info_hash}/{file_idx}/subtitles/{stream_index}",
            axum::routing::get(stream_embedded_subtitles),
        )
        .route(
            "/stream/{info_hash}/{file_idx}/remux",
            axum::routing::post(stream_remux_hls),
        )
        .route("/hls/{session_id}/{file}", axum::routing::get(hls_serve))
        .route("/hls/{session_id}", axum::routing::delete(hls_stop))
        .route("/image/{*path}", axum::routing::get(image_proxy))
        .route("/files/{*path}", axum::routing::get(serve_file))
}

#[derive(Deserialize, IntoParams)]
struct SearchParams {
    q: String,
}

#[utoipa::path(get, path = "/search",
    params(SearchParams),
    responses((status = 200, body = Vec<SearchResult>))
)]
async fn search(
    State(ctx): State<AppContext>,
    Query(params): Query<SearchParams>,
) -> Result<Json<Vec<SearchResult>>, AppError> {
    let config = ctx.config.clone();
    let tmdb = TmdbClient::new(&config, ctx.http.clone());
    let results = tmdb.search(&params.q).await?;
    Ok(Json(results))
}

#[utoipa::path(get, path = "/movie/{id}", responses((status = 200, body = MediaItem)))]
async fn movie_details(
    State(ctx): State<AppContext>,
    Path(id): Path<i64>,
) -> Result<Json<MediaItem>, AppError> {
    let config = ctx.config.clone();
    let tmdb = TmdbClient::new(&config, ctx.http.clone());
    let item = tmdb.details(MediaType::Movie, id).await?;
    Ok(Json(item))
}

#[utoipa::path(get, path = "/tv/{id}", responses((status = 200, body = MediaItem)))]
async fn tv_details(
    State(ctx): State<AppContext>,
    Path(id): Path<i64>,
) -> Result<Json<MediaItem>, AppError> {
    let config = ctx.config.clone();
    let tmdb = TmdbClient::new(&config, ctx.http.clone());
    let item = tmdb.details(MediaType::Tv, id).await?;
    Ok(Json(item))
}

#[utoipa::path(get, path = "/streams/movie/{id}", responses((status = 200, body = Vec<Stream>)))]
async fn movie_streams(
    State(ctx): State<AppContext>,
    Path(id): Path<i64>,
) -> Result<Json<Vec<Stream>>, AppError> {
    let config = ctx.config.clone();

    // Get IMDB ID from TMDB
    let tmdb = TmdbClient::new(&config, ctx.http.clone());
    let item = tmdb.details(MediaType::Movie, id).await?;
    let imdb_id = item
        .imdb_id
        .ok_or_else(|| Error::Generic("No IMDB ID found for this movie".into()))?;

    let path = format!("movie/{}", imdb_id);
    let streams = crate::streams::aggregate(&ctx.http, &config.stream_sources, &path).await;

    Ok(Json(streams))
}

#[utoipa::path(get, path = "/streams/tv/{id}/{season}/{episode}", responses((status = 200, body = Vec<Stream>)))]
async fn tv_streams(
    State(ctx): State<AppContext>,
    Path((id, season, episode)): Path<(i64, i64, i64)>,
) -> Result<Json<Vec<Stream>>, AppError> {
    let config = ctx.config.clone();

    let tmdb = TmdbClient::new(&config, ctx.http.clone());
    let item = tmdb.details(MediaType::Tv, id).await?;
    let imdb_id = item
        .imdb_id
        .ok_or_else(|| Error::Generic("No IMDB ID found for this show".into()))?;

    let path = format!("series/{}:{}:{}", imdb_id, season, episode);
    let streams = crate::streams::aggregate(&ctx.http, &config.stream_sources, &path).await;

    Ok(Json(streams))
}

#[derive(Serialize, ToSchema)]
struct StartStreamResponse {
    url: String,
    local: bool,
}

#[utoipa::path(post, path = "/streams/start/{info_hash}/{file_idx}",
    responses((status = 200, body = StartStreamResponse))
)]
async fn start_stream(
    State(ctx): State<AppContext>,
    Path((info_hash, file_idx)): Path<(String, i64)>,
) -> Result<Json<StartStreamResponse>, AppError> {
    // Start torrent via native engine (idempotent — if already downloaded,
    // librqbit's fastresume picks it up instantly from disk)
    let engine = TorrentEngine::get();
    engine
        .start(&info_hash, file_idx as usize, &ctx.config)
        .await?;

    let url = format!("/api/stream/{}/{}", info_hash, file_idx);
    Ok(Json(StartStreamResponse { url, local: false }))
}

#[utoipa::path(get, path = "/subtitles/movie/{id}", responses((status = 200, body = Vec<SubtitleTrack>)))]
async fn movie_subtitles(
    State(ctx): State<AppContext>,
    Path(id): Path<i64>,
) -> Result<Json<Vec<SubtitleTrack>>, AppError> {
    let config = ctx.config.clone();
    let tmdb = TmdbClient::new(&config, ctx.http.clone());
    let item = tmdb.details(MediaType::Movie, id).await?;
    let imdb_id = item
        .imdb_id
        .ok_or_else(|| Error::Generic("No IMDB ID found".into()))?;

    let path = format!("movie/{}", imdb_id);
    let tracks = crate::subtitles::fetch_tracks(&ctx.http, &path, &config.subtitle_languages).await;
    Ok(Json(tracks))
}

#[utoipa::path(get, path = "/subtitles/tv/{id}/{season}/{episode}", responses((status = 200, body = Vec<SubtitleTrack>)))]
async fn tv_subtitles(
    State(ctx): State<AppContext>,
    Path((id, season, episode)): Path<(i64, i64, i64)>,
) -> Result<Json<Vec<SubtitleTrack>>, AppError> {
    let config = ctx.config.clone();
    let tmdb = TmdbClient::new(&config, ctx.http.clone());
    let item = tmdb.details(MediaType::Tv, id).await?;
    let imdb_id = item
        .imdb_id
        .ok_or_else(|| Error::Generic("No IMDB ID found".into()))?;

    let path = format!("series/{}:{}:{}", imdb_id, season, episode);
    let tracks = crate::subtitles::fetch_tracks(&ctx.http, &path, &config.subtitle_languages).await;
    Ok(Json(tracks))
}

#[derive(Deserialize, IntoParams)]
struct SubtitleCueParams {
    /// URL of the SRT subtitle file
    url: String,
}

#[utoipa::path(get, path = "/subtitles/cues",
    params(SubtitleCueParams),
    responses((status = 200, body = Vec<SubtitleCue>))
)]
async fn subtitle_cues(
    State(ctx): State<AppContext>,
    Query(params): Query<SubtitleCueParams>,
) -> Result<Json<Vec<SubtitleCue>>, AppError> {
    let cues = crate::subtitles::fetch_cues(&ctx.http, &params.url).await;
    Ok(Json(cues))
}

async fn image_proxy(
    State(ctx): State<AppContext>,
    Path(path): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let path = if path.starts_with("w") || path.starts_with("original") {
        path
    } else {
        format!("original/{path}")
    };

    // Disk cache: {storage}/cache/images/{path}
    let cache_path = ctx.storage.join("cache/images").join(&path);

    // Serve from cache if available
    if cache_path.exists() {
        let bytes = tokio::fs::read(&cache_path).await?;
        let content_type = mime_guess::from_path(&cache_path)
            .first_or_octet_stream()
            .to_string();
        return Ok((
            [
                (header::CONTENT_TYPE, content_type),
                (header::CACHE_CONTROL, "public, max-age=604800".into()),
            ],
            bytes.into(),
        ));
    }

    // Fetch from TMDB
    let url = format!("https://image.tmdb.org/t/p/{path}");
    let res = ctx
        .http
        .get(&url)
        .send()
        .await?
        .error_for_status()
        .map_err(|e| Error::Generic(e.to_string()))?;

    let content_type = res
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("image/jpeg")
        .to_string();

    let bytes = res.bytes().await?;

    // Write to disk cache
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
    ))
}

// ── Browse endpoints ──

#[utoipa::path(get, path = "/trending", responses((status = 200, body = Vec<SearchResult>)))]
async fn trending(State(ctx): State<AppContext>) -> Result<Json<Vec<SearchResult>>, AppError> {
    let config = ctx.config.clone();
    let tmdb = TmdbClient::new(&config, ctx.http.clone());
    let results = tmdb.trending().await?;
    Ok(Json(results))
}

#[utoipa::path(get, path = "/similar/{type}/{id}", responses((status = 200, body = Vec<SearchResult>)))]
async fn similar(
    State(ctx): State<AppContext>,
    Path((media_type, id)): Path<(String, i64)>,
) -> Result<Json<Vec<SearchResult>>, AppError> {
    let config = ctx.config.clone();
    let tmdb = TmdbClient::new(&config, ctx.http.clone());
    let mt = match media_type.as_str() {
        "movie" => MediaType::Movie,
        "tv" => MediaType::Tv,
        _ => return Err(Error::Generic("Invalid media type".into()).into()),
    };
    let results = tmdb.similar(mt, id).await?;
    Ok(Json(results))
}

// ── Watch history ──

#[derive(Deserialize, Serialize, ToSchema)]
struct RecordWatchRequest {
    media_type: String,
    tmdb_id: i64,
    title: String,
    poster_path: Option<String>,
    season: Option<i64>,
    episode: Option<i64>,
    info_hash: Option<String>,
    file_idx: Option<i64>,
    progress: Option<f64>,
    duration: Option<f64>,
}

#[derive(Serialize, ToSchema, sqlx::FromRow)]
struct WatchHistoryItem {
    media_type: String,
    tmdb_id: i64,
    title: String,
    poster_path: Option<String>,
    season: i64,
    episode: i64,
    info_hash: Option<String>,
    file_idx: i64,
    progress: f64,
    duration: f64,
    last_watched: String,
}

#[utoipa::path(post, path = "/watch",
    request_body = RecordWatchRequest,
    responses((status = 204))
)]
async fn record_watch(
    State(ctx): State<AppContext>,
    Json(body): Json<RecordWatchRequest>,
) -> Result<StatusCode, AppError> {
    sqlx::query(
        "INSERT INTO watch_history (media_type, tmdb_id, title, poster_path, season, episode, info_hash, file_idx, progress, duration)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(media_type, tmdb_id)
         DO UPDATE SET title = excluded.title, poster_path = excluded.poster_path, season = excluded.season, episode = excluded.episode, info_hash = excluded.info_hash, file_idx = excluded.file_idx, progress = excluded.progress, duration = excluded.duration, last_watched = datetime('now')"
    )
    .bind(&body.media_type)
    .bind(body.tmdb_id)
    .bind(&body.title)
    .bind(&body.poster_path)
    .bind(body.season.unwrap_or(0))
    .bind(body.episode.unwrap_or(0))
    .bind(&body.info_hash)
    .bind(body.file_idx.unwrap_or(0))
    .bind(body.progress.unwrap_or(0.0))
    .bind(body.duration.unwrap_or(0.0))
    .execute(&ctx.db)
    .await
    .map_err(|e| Error::Generic(e.to_string()))?;

    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(get, path = "/watch/history", responses((status = 200, body = Vec<WatchHistoryItem>)))]
async fn watch_history(
    State(ctx): State<AppContext>,
) -> Result<Json<Vec<WatchHistoryItem>>, AppError> {
    let items = sqlx::query_as::<_, WatchHistoryItem>(
        "SELECT media_type, tmdb_id, title, poster_path, season, episode, info_hash, file_idx, progress, duration, last_watched
         FROM watch_history ORDER BY last_watched DESC LIMIT 20"
    )
    .fetch_all(&ctx.db)
    .await
    .map_err(|e| Error::Generic(e.to_string()))?;

    Ok(Json(items))
}

// ── Collections (watchlist, favorites, etc.) ──

#[derive(Deserialize, Serialize, ToSchema)]
struct CollectionRequest {
    collection: String,
    media_type: String,
    tmdb_id: i64,
    title: String,
    poster_path: Option<String>,
}

#[derive(Serialize, ToSchema, sqlx::FromRow)]
struct CollectionItem {
    collection: String,
    media_type: String,
    tmdb_id: i64,
    title: String,
    poster_path: Option<String>,
    added_at: String,
    position: i64,
}

#[derive(Serialize, ToSchema)]
struct CollectionStatus {
    in_collection: bool,
}

#[derive(Serialize, ToSchema, sqlx::FromRow)]
struct CollectionDef {
    slug: String,
    title: String,
    kind: String,
    created_at: String,
    // 0/1 flags (sqlite has no native bool; kept as ints for AnyPool safety)
    system: i64,
    hidden: i64,
}

#[derive(Deserialize, ToSchema)]
struct VisibilityRequest {
    hidden: bool,
}

#[derive(Deserialize, Serialize, ToSchema)]
struct CreateCollectionRequest {
    slug: String,
    title: String,
    kind: String,
}

#[derive(Deserialize, ToSchema)]
struct ReorderItem {
    media_type: String,
    tmdb_id: i64,
}

#[derive(Deserialize, ToSchema)]
struct ReorderRequest {
    items: Vec<ReorderItem>,
}

#[derive(Deserialize, ToSchema)]
struct ReorderCollectionsRequest {
    slugs: Vec<String>,
}

#[utoipa::path(post, path = "/collection",
    request_body = CollectionRequest,
    responses((status = 204))
)]
async fn add_to_collection(
    State(ctx): State<AppContext>,
    Json(body): Json<CollectionRequest>,
) -> Result<StatusCode, AppError> {
    sqlx::query(
        "INSERT INTO collections (collection, media_type, tmdb_id, title, poster_path)
         VALUES (?, ?, ?, ?, ?)
         ON CONFLICT(collection, media_type, tmdb_id) DO UPDATE SET title = excluded.title, poster_path = excluded.poster_path"
    )
    .bind(&body.collection)
    .bind(&body.media_type)
    .bind(body.tmdb_id)
    .bind(&body.title)
    .bind(&body.poster_path)
    .execute(&ctx.db)
    .await
    .map_err(|e| Error::Generic(e.to_string()))?;

    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(delete, path = "/collection/{collection}/{type}/{id}", responses((status = 204)))]
async fn remove_from_collection(
    State(ctx): State<AppContext>,
    Path((collection, media_type, id)): Path<(String, String, i64)>,
) -> Result<StatusCode, AppError> {
    sqlx::query("DELETE FROM collections WHERE collection = ? AND media_type = ? AND tmdb_id = ?")
        .bind(&collection)
        .bind(&media_type)
        .bind(id)
        .execute(&ctx.db)
        .await
        .map_err(|e| Error::Generic(e.to_string()))?;

    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(get, path = "/collection/{collection}", responses((status = 200, body = Vec<CollectionItem>)))]
async fn get_collection(
    State(ctx): State<AppContext>,
    Path(collection): Path<String>,
) -> Result<Json<Vec<CollectionItem>>, AppError> {
    let items = sqlx::query_as::<_, CollectionItem>(
        "SELECT collection, media_type, tmdb_id, title, poster_path, added_at, position
         FROM collections WHERE collection = ? ORDER BY position ASC, added_at DESC",
    )
    .bind(&collection)
    .fetch_all(&ctx.db)
    .await
    .map_err(|e| Error::Generic(e.to_string()))?;

    Ok(Json(items))
}

#[utoipa::path(get, path = "/collection/{collection}/{type}/{id}", responses((status = 200, body = CollectionStatus)))]
async fn is_in_collection(
    State(ctx): State<AppContext>,
    Path((collection, media_type, id)): Path<(String, String, i64)>,
) -> Result<Json<CollectionStatus>, AppError> {
    let count: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM collections WHERE collection = ? AND media_type = ? AND tmdb_id = ?",
    )
    .bind(&collection)
    .bind(&media_type)
    .bind(id)
    .fetch_one(&ctx.db)
    .await
    .map_err(|e| Error::Generic(e.to_string()))?;

    Ok(Json(CollectionStatus {
        in_collection: count.0 > 0,
    }))
}

#[utoipa::path(get, path = "/collections", responses((status = 200, body = Vec<CollectionDef>)))]
async fn list_collection_defs(
    State(ctx): State<AppContext>,
) -> Result<Json<Vec<CollectionDef>>, AppError> {
    let defs = sqlx::query_as::<_, CollectionDef>(
        "SELECT slug, title, kind, created_at, system, hidden FROM collection_meta ORDER BY position ASC, created_at ASC",
    )
    .fetch_all(&ctx.db)
    .await
    .map_err(|e| Error::Generic(e.to_string()))?;

    Ok(Json(defs))
}

#[utoipa::path(post, path = "/collections",
    request_body = CreateCollectionRequest,
    responses((status = 204))
)]
async fn create_collection_def(
    State(ctx): State<AppContext>,
    Json(body): Json<CreateCollectionRequest>,
) -> Result<StatusCode, AppError> {
    sqlx::query(
        "INSERT INTO collection_meta (slug, title, kind) VALUES (?, ?, ?)
         ON CONFLICT(slug) DO UPDATE SET title = excluded.title, kind = excluded.kind",
    )
    .bind(&body.slug)
    .bind(&body.title)
    .bind(&body.kind)
    .execute(&ctx.db)
    .await
    .map_err(|e| Error::Generic(e.to_string()))?;

    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(delete, path = "/collections/{slug}", responses((status = 204)))]
async fn delete_collection_def(
    State(ctx): State<AppContext>,
    Path(slug): Path<String>,
) -> Result<StatusCode, AppError> {
    let system: Option<(i64,)> =
        sqlx::query_as("SELECT system FROM collection_meta WHERE slug = ?")
            .bind(&slug)
            .fetch_optional(&ctx.db)
            .await
            .map_err(|e| Error::Generic(e.to_string()))?;

    if matches!(system, Some((1,))) {
        return Err(
            Error::Generic("system collections cannot be deleted".to_string()).into(),
        );
    }

    sqlx::query("DELETE FROM collection_meta WHERE slug = ?")
        .bind(&slug)
        .execute(&ctx.db)
        .await
        .map_err(|e| Error::Generic(e.to_string()))?;

    sqlx::query("DELETE FROM collections WHERE collection = ?")
        .bind(&slug)
        .execute(&ctx.db)
        .await
        .map_err(|e| Error::Generic(e.to_string()))?;

    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(post, path = "/collections/{slug}/visibility",
    request_body = VisibilityRequest,
    responses((status = 204))
)]
async fn set_collection_visibility(
    State(ctx): State<AppContext>,
    Path(slug): Path<String>,
    Json(body): Json<VisibilityRequest>,
) -> Result<StatusCode, AppError> {
    sqlx::query("UPDATE collection_meta SET hidden = ? WHERE slug = ?")
        .bind(body.hidden as i64)
        .bind(&slug)
        .execute(&ctx.db)
        .await
        .map_err(|e| Error::Generic(e.to_string()))?;

    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(post, path = "/collections/reorder",
    request_body = ReorderCollectionsRequest,
    responses((status = 204))
)]
async fn reorder_collection_defs(
    State(ctx): State<AppContext>,
    Json(body): Json<ReorderCollectionsRequest>,
) -> Result<StatusCode, AppError> {
    for (idx, slug) in body.slugs.iter().enumerate() {
        sqlx::query("UPDATE collection_meta SET position = ? WHERE slug = ?")
            .bind(idx as i64)
            .bind(slug)
            .execute(&ctx.db)
            .await
            .map_err(|e| Error::Generic(e.to_string()))?;
    }

    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(post, path = "/collection/{collection}/reorder",
    request_body = ReorderRequest,
    responses((status = 204))
)]
async fn reorder_collection(
    State(ctx): State<AppContext>,
    Path(collection): Path<String>,
    Json(body): Json<ReorderRequest>,
) -> Result<StatusCode, AppError> {
    for (idx, item) in body.items.iter().enumerate() {
        sqlx::query(
            "UPDATE collections SET position = ?
             WHERE collection = ? AND media_type = ? AND tmdb_id = ?",
        )
        .bind(idx as i64)
        .bind(&collection)
        .bind(&item.media_type)
        .bind(item.tmdb_id)
        .execute(&ctx.db)
        .await
        .map_err(|e| Error::Generic(e.to_string()))?;
    }

    Ok(StatusCode::NO_CONTENT)
}

// ── Downloads ──

#[derive(Deserialize, Serialize, ToSchema)]
struct EnqueueDownloadRequest {
    media_type: String,
    tmdb_id: i64,
    title: String,
    poster_path: Option<String>,
    #[serde(default)]
    season: i64,
    #[serde(default)]
    episode: i64,
    resolution: String,
    info_hash: Option<String>,
    file_idx: Option<i64>,
}

#[utoipa::path(post, path = "/downloads",
    request_body = EnqueueDownloadRequest,
    responses((status = 204))
)]
async fn enqueue_download(
    State(ctx): State<AppContext>,
    Json(body): Json<EnqueueDownloadRequest>,
) -> Result<StatusCode, AppError> {
    let (info_hash, file_idx) = if let (Some(hash), Some(idx)) = (&body.info_hash, body.file_idx) {
        (hash.clone(), idx)
    } else {
        // Auto-select best stream at requested resolution
        let config = ctx.config.clone();
        let tmdb = TmdbClient::new(&config, ctx.http.clone());
        let mt = match body.media_type.as_str() {
            "movie" => MediaType::Movie,
            "tv" => MediaType::Tv,
            _ => return Err(Error::Generic("Invalid media type".into()).into()),
        };
        let item = tmdb.details(mt, body.tmdb_id).await?;
        let imdb_id = item
            .imdb_id
            .ok_or_else(|| Error::Generic("No IMDB ID found".into()))?;

        let path = if body.media_type == "tv" {
            format!("series/{}:{}:{}", imdb_id, body.season, body.episode)
        } else {
            format!("movie/{}", imdb_id)
        };

        let all_streams = streams::aggregate(&ctx.http, &config.stream_sources, &path).await;
        let stream = all_streams
            .iter()
            .find(|s| s.resolution.as_deref() == Some(&body.resolution))
            .or_else(|| all_streams.first())
            .ok_or_else(|| Error::Generic("No streams found".into()))?;

        (stream.info_hash.clone(), stream.file_idx)
    };

    let file_path = if body.media_type == "tv" {
        format!("tv/{}/s{}e{}.mp4", body.tmdb_id, body.season, body.episode)
    } else {
        format!("movies/{}.mp4", body.tmdb_id)
    };

    sqlx::query(
        "INSERT INTO downloads (media_type, tmdb_id, title, poster_path, season, episode, resolution, info_hash, file_idx, file_path)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(media_type, tmdb_id, season, episode) DO UPDATE SET
           info_hash = excluded.info_hash, file_idx = excluded.file_idx, resolution = excluded.resolution,
           file_path = excluded.file_path, status = 'queued', error = NULL,
           downloaded_bytes = 0, total_bytes = NULL, completed_at = NULL"
    )
    .bind(&body.media_type)
    .bind(body.tmdb_id)
    .bind(&body.title)
    .bind(&body.poster_path)
    .bind(body.season)
    .bind(body.episode)
    .bind(&body.resolution)
    .bind(&info_hash)
    .bind(file_idx)
    .bind(&file_path)
    .execute(&ctx.db)
    .await
    .map_err(|e| Error::Generic(e.to_string()))?;

    ctx.events
        .publish("download:enqueue", serde_json::json!({}));

    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(get, path = "/downloads", responses((status = 200, body = Vec<Download>)))]
async fn list_downloads(State(ctx): State<AppContext>) -> Result<Json<Vec<Download>>, AppError> {
    let items = sqlx::query_as::<_, Download>("SELECT * FROM downloads ORDER BY created_at DESC")
        .fetch_all(&ctx.db)
        .await
        .map_err(|e| Error::Generic(e.to_string()))?;

    Ok(Json(items))
}

#[utoipa::path(delete, path = "/downloads/{id}", responses((status = 204)))]
async fn delete_download(
    State(ctx): State<AppContext>,
    Path(id): Path<i64>,
) -> Result<StatusCode, AppError> {
    // Get the download to find the file path
    let dl: Option<Download> = sqlx::query_as("SELECT * FROM downloads WHERE id = ?")
        .bind(id)
        .fetch_optional(&ctx.db)
        .await
        .map_err(|e| Error::Generic(e.to_string()))?;

    if let Some(dl) = dl {
        if dl.status == "downloading" {
            // Mark as cancelled so the download worker stops
            sqlx::query("UPDATE downloads SET status = 'cancelled' WHERE id = ?")
                .bind(id)
                .execute(&ctx.db)
                .await
                .map_err(|e| Error::Generic(e.to_string()))?;
        }

        // Stop the torrent and delete its files
        let engine = TorrentEngine::get();
        engine.stop_and_delete(&dl.info_hash).await;

        sqlx::query("DELETE FROM downloads WHERE id = ?")
            .bind(id)
            .execute(&ctx.db)
            .await
            .map_err(|e| Error::Generic(e.to_string()))?;
    }

    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize, ToSchema)]
struct ResolutionEstimate {
    resolution: String,
    size_bytes: Option<u64>,
    size_display: Option<String>,
    streams_count: usize,
}

#[utoipa::path(get, path = "/downloads/estimate/{media_type}/{tmdb_id}",
    responses((status = 200, body = Vec<ResolutionEstimate>))
)]
async fn estimate_download(
    State(ctx): State<AppContext>,
    Path((media_type, tmdb_id)): Path<(String, i64)>,
) -> Result<Json<Vec<ResolutionEstimate>>, AppError> {
    let config = ctx.config.clone();
    let tmdb = TmdbClient::new(&config, ctx.http.clone());
    let mt = match media_type.as_str() {
        "movie" => MediaType::Movie,
        "tv" => MediaType::Tv,
        _ => return Err(Error::Generic("Invalid media type".into()).into()),
    };
    let item = tmdb.details(mt, tmdb_id).await?;
    let imdb_id = item
        .imdb_id
        .ok_or_else(|| Error::Generic("No IMDB ID found".into()))?;

    let path = if media_type == "tv" {
        // Estimate from first episode of first season
        format!("series/{}:1:1", imdb_id)
    } else {
        format!("movie/{}", imdb_id)
    };

    let all_streams = streams::aggregate(&ctx.http, &config.stream_sources, &path).await;

    // Group by resolution, pick best stream per resolution for size estimate
    let mut seen = std::collections::HashMap::<String, (Option<u64>, Option<String>, usize)>::new();
    for s in &all_streams {
        let Some(res) = s.resolution.clone() else {
            continue;
        };
        let entry = seen.entry(res).or_insert((None, None, 0));
        entry.2 += 1;
        // Use the first (best-scored) stream's size as estimate
        if entry.0.is_none() {
            entry.0 = s.size_bytes;
            entry.1 = s.size_display.clone();
        }
    }

    let order = |r: &str| -> u32 {
        match r {
            "4K" | "2160p" => 4,
            "1080p" => 3,
            "720p" => 2,
            "480p" => 1,
            _ => 0,
        }
    };

    let mut estimates: Vec<ResolutionEstimate> = seen
        .into_iter()
        .map(
            |(resolution, (size_bytes, size_display, streams_count))| ResolutionEstimate {
                resolution,
                size_bytes,
                size_display,
                streams_count,
            },
        )
        .collect();
    estimates.sort_by(|a, b| order(&b.resolution).cmp(&order(&a.resolution)));

    Ok(Json(estimates))
}

#[derive(Serialize, ToSchema)]
struct StreamStatsResponse {
    progress_bytes: u64,
    total_bytes: u64,
    download_speed_mbps: f64,
    peers: usize,
    finished: bool,
}

async fn stream_stats(
    Path(info_hash): Path<String>,
) -> Result<Json<StreamStatsResponse>, AppError> {
    let engine = TorrentEngine::get();
    let stats = engine.stats(&info_hash)?;
    let (download_speed_mbps, peers) = match &stats.live {
        Some(live) => (live.download_speed.mbps, live.snapshot.peer_stats.live),
        None => (0.0, 0),
    };
    Ok(Json(StreamStatsResponse {
        progress_bytes: stats.progress_bytes,
        total_bytes: stats.total_bytes,
        download_speed_mbps,
        peers,
        finished: stats.finished,
    }))
}

async fn stream_pieces(
    Path((info_hash, file_idx)): Path<(String, usize)>,
) -> Result<Json<Vec<u8>>, AppError> {
    let engine = TorrentEngine::get();
    let pieces = engine.piece_map(&info_hash, file_idx, 200)?;
    Ok(Json(pieces))
}

async fn stream_file(
    State(ctx): State<AppContext>,
    Path((info_hash, file_idx)): Path<(String, usize)>,
    req: axum::http::Request<axum::body::Body>,
) -> Result<axum::response::Response, AppError> {
    let engine = TorrentEngine::get();

    // Ensure the torrent is started (also registers streaming priority)
    engine.start(&info_hash, file_idx, &ctx.config).await?;

    // Use librqbit's FileStream which blocks on missing pieces and
    // prioritizes sequential download. This is correct because the stream
    // only covers THIS file's pieces — it won't block on pieces belonging
    // to other files in the torrent.
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
) -> Result<Json<serde_json::Value>, AppError> {
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

async fn stream_embedded_subtitles(
    Path((info_hash, file_idx, stream_index)): Path<(String, usize, usize)>,
) -> Result<Json<Vec<SubtitleCue>>, AppError> {
    let engine = TorrentEngine::get();
    let path = engine.file_path(&info_hash, file_idx)?;
    let cues = TorrentEngine::extract_subtitle_cues(&path, stream_index).await;
    Ok(Json(cues))
}

#[derive(Deserialize, IntoParams)]
struct RemuxParams {
    #[serde(default)]
    audio: usize,
    #[serde(default)]
    t: f64,
}

#[derive(Serialize, ToSchema)]
struct RemuxResponse {
    session_id: String,
    playlist_url: String,
}

async fn stream_remux_hls(
    State(ctx): State<AppContext>,
    Path((info_hash, file_idx)): Path<(String, usize)>,
    Query(params): Query<RemuxParams>,
) -> Result<Json<RemuxResponse>, AppError> {
    let engine = TorrentEngine::get();
    engine.start(&info_hash, file_idx, &ctx.config).await?;

    let (session_id, playlist_url) = crate::hls::start_session(
        &ctx.storage,
        &ctx.config,
        &info_hash,
        file_idx,
        params.audio,
        params.t,
    )
    .await?;

    Ok(Json(RemuxResponse {
        session_id,
        playlist_url,
    }))
}

async fn hls_serve(
    Path((session_id, file)): Path<(String, String)>,
) -> Result<axum::response::Response, AppError> {
    // Validate: no path traversal
    if file.contains("..") || file.contains('/') {
        return Err(Error::Generic("Invalid path".into()).into());
    }

    let dir = crate::hls::session_dir(&session_id)
        .await
        .ok_or_else(|| Error::Generic("HLS session not found".into()))?;

    crate::hls::touch(&session_id).await;

    let full_path = dir.join(&file);
    let bytes = match tokio::fs::read(&full_path).await {
        Ok(b) => b,
        Err(_) => {
            // Check if ffmpeg crashed — return the real error instead of "file not found"
            if let Some(error) = crate::hls::session_error(&session_id).await {
                return Err(
                    Error::Generic(format!("Stream failed (ffmpeg exited): {error}")).into(),
                );
            }
            return Err(Error::Generic(format!("HLS file not found: {file}")).into());
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

async fn hls_stop(Path(session_id): Path<String>) -> StatusCode {
    crate::hls::stop_session(&session_id).await;
    StatusCode::NO_CONTENT
}

async fn serve_file(
    State(ctx): State<AppContext>,
    Path(path): Path<String>,
    req: axum::http::Request<axum::body::Body>,
) -> Result<axum::response::Response, AppError> {
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
) -> Result<axum::response::Response, AppError> {
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
        if start > 0 {
            if let Err(e) = reader.as_mut().seek(std::io::SeekFrom::Start(start)).await {
                yield Err(e);
                return;
            }
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

struct AppError(Error);

impl IntoResponse for AppError {
    fn into_response(self) -> axum::response::Response {
        (StatusCode::INTERNAL_SERVER_ERROR, self.0.to_string()).into_response()
    }
}

impl<E: Into<Error>> From<E> for AppError {
    fn from(e: E) -> Self {
        AppError(e.into())
    }
}
