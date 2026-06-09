//! Download lifecycle. The database is the source of truth; this module
//! actuates against `TorrentEngine` in response to commands and writes
//! observed progress back.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Semaphore, mpsc};
use tokio_util::sync::CancellationToken;

use crate::app::Pool;
use crate::config::Config;
use crate::tmdb::{self, TmdbClient};
use crate::torrent::TorrentEngine;

// ── Public types ─────────────────────────────────────────────────────────

#[draad::ty]
#[derive(sqlx::Type, PartialEq)]
#[sqlx(type_name = "download_status", rename_all = "lowercase")]
pub enum DownloadStatus {
    Queued,
    Downloading,
    Paused,
    Completed,
    Cancelled,
    Failed,
}

#[draad::ty]
pub struct DownloadMeta {
    pub media_type: tmdb::MediaType,
    pub tmdb_id: i64,
    pub title: String,
    pub poster_path: Option<String>,
    pub season: i64,
    pub episode: i64,
    pub resolution: Option<String>,
}

#[draad::ty]
pub struct Download {
    pub id: i32,
    pub info_hash: String,
    pub file_idx: i64,
    pub name: Option<String>,
    pub total_bytes: Option<i64>,
    pub downloaded_bytes: i64,
    pub status: DownloadStatus,
    pub error: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
    pub meta: Option<DownloadMeta>,
}

/// Flat row mapped from the LEFT JOIN'd select. Made `pub(crate)` so the
/// query_as! macro can construct it; collapsed to [`Download`] via `From`.
#[derive(Debug)]
struct DownloadRow {
    id: i32,
    info_hash: String,
    file_idx: i32,
    name: Option<String>,
    total_bytes: Option<i64>,
    downloaded_bytes: i64,
    status: DownloadStatus,
    error: Option<String>,
    created_at: chrono::DateTime<chrono::Utc>,
    completed_at: Option<chrono::DateTime<chrono::Utc>>,
    media_type: Option<tmdb::MediaType>,
    tmdb_id: Option<i64>,
    title: Option<String>,
    poster_path: Option<String>,
    season: Option<i32>,
    episode: Option<i32>,
    resolution: Option<String>,
}

impl From<DownloadRow> for Download {
    fn from(r: DownloadRow) -> Self {
        let meta = r.media_type.map(|mt| DownloadMeta {
            media_type: mt,
            tmdb_id: r.tmdb_id.unwrap_or(0),
            title: r.title.unwrap_or_default(),
            poster_path: r.poster_path,
            season: r.season.unwrap_or(0) as i64,
            episode: r.episode.unwrap_or(0) as i64,
            resolution: r.resolution,
        });
        Download {
            id: r.id,
            info_hash: r.info_hash,
            file_idx: r.file_idx as i64,
            name: r.name,
            total_bytes: r.total_bytes,
            downloaded_bytes: r.downloaded_bytes,
            status: r.status,
            error: r.error,
            created_at: r.created_at,
            completed_at: r.completed_at,
            meta,
        }
    }
}

// ── Commands & Handle ────────────────────────────────────────────────────

#[derive(Debug)]
pub enum DownloadCommand {
    /// Start (or resume) a download. Valid from queued/paused/cancelled/failed.
    Start(i32),
    /// Stop downloading, keep files. Intent: temporary hold.
    Pause(i32),
    /// Stop downloading, keep files. Intent: user no longer wants this download.
    Cancel(i32),
    /// Stop downloading, wipe files + row. Always destructive.
    Remove(i32),
    /// Look for queued rows that can be started given current capacity.
    Refresh,
    /// Internal: a supervisor task finished and is no longer holding its slot.
    Done(i32),
}

#[derive(Clone)]
pub struct Handle {
    tx: mpsc::Sender<DownloadCommand>,
}

impl Handle {
    pub async fn send(&self, cmd: DownloadCommand) {
        if let Err(err) = self.tx.send(cmd).await {
            tracing::error!(?err, "Download command channel closed");
        }
    }
}

// ── Manager ──────────────────────────────────────────────────────────────

pub struct DownloadManager {
    db: Pool,
    config: Arc<Config>,
    http: reqwest::Client,
    rx: mpsc::Receiver<DownloadCommand>,
    tx_self: mpsc::Sender<DownloadCommand>,
    semaphore: Arc<Semaphore>,
    supervisors: HashMap<i32, CancellationToken>,
}

impl DownloadManager {
    pub fn new(db: Pool, config: Arc<Config>, http: reqwest::Client) -> (Handle, Self) {
        let permits = config.max_concurrent_downloads;
        let (tx, rx) = mpsc::channel::<DownloadCommand>(256);
        let mgr = Self {
            db,
            config,
            http,
            rx,
            tx_self: tx.clone(),
            semaphore: Arc::new(Semaphore::new(permits)),
            supervisors: HashMap::new(),
        };
        (Handle { tx }, mgr)
    }

    pub async fn run(mut self) {
        if let Err(err) = self.boot_recover().await {
            tracing::error!(?err, "Download manager boot recovery failed");
        }
        tracing::info!("Download manager started");

        while let Some(cmd) = self.rx.recv().await {
            match cmd {
                DownloadCommand::Start(id) => self.handle_start(id).await,
                DownloadCommand::Pause(id) => self.handle_pause(id).await,
                DownloadCommand::Cancel(id) => self.handle_cancel(id).await,
                DownloadCommand::Remove(id) => self.handle_remove(id).await,
                DownloadCommand::Refresh => self.handle_refresh().await,
                DownloadCommand::Done(id) => self.handle_done(id).await,
            }
        }
    }

    async fn boot_recover(&mut self) -> crate::app::Result<()> {
        // Anything left as `downloading` was interrupted by the previous run;
        // demote to `queued` so handle_refresh re-fetches it normally.
        let reset =
            sqlx::query!("UPDATE downloads SET status = 'queued' WHERE status = 'downloading'")
                .execute(&self.db)
                .await
                .map_err(crate::app::Error::DatabaseError)?;
        if reset.rows_affected() > 0 {
            tracing::info!(
                count = reset.rows_affected(),
                "Reset interrupted downloads to queued"
            );
        }
        self.handle_refresh().await;
        Ok(())
    }

    async fn handle_start(&mut self, id: i32) {
        if self.supervisors.contains_key(&id) {
            return;
        }
        let permit = match self.semaphore.clone().try_acquire_owned() {
            Ok(p) => p,
            Err(_) => {
                // No capacity — leave queued; handle_refresh will retry as
                // supervisors finish and emit Done.
                return;
            }
        };
        if let Err(err) = sqlx::query!(
            "UPDATE downloads SET status = 'downloading', error = NULL WHERE id = $1",
            id
        )
        .execute(&self.db)
        .await
        {
            tracing::error!(?err, id, "Failed to mark download as downloading");
            return;
        }

        let cancel = CancellationToken::new();
        self.supervisors.insert(id, cancel.clone());
        let db = self.db.clone();
        let config = self.config.clone();
        let http = self.http.clone();
        let tx_self = self.tx_self.clone();
        tokio::spawn(async move {
            supervisor(db, config, http, id, cancel).await;
            drop(permit);
            let _ = tx_self.send(DownloadCommand::Done(id)).await;
        });
    }

    async fn handle_pause(&mut self, id: i32) {
        if let Some(cancel) = self.supervisors.remove(&id) {
            cancel.cancel();
        }
        if let Some(hash) = self.fetch_info_hash(id).await
            && let Err(err) = TorrentEngine::get().pause(&hash).await
        {
            tracing::debug!(?err, id, "Pause: engine.pause returned error");
        }
        if let Err(err) = sqlx::query!("UPDATE downloads SET status = 'paused' WHERE id = $1", id)
            .execute(&self.db)
            .await
        {
            tracing::error!(?err, id, "Failed to mark download as paused");
        }
    }

    async fn handle_cancel(&mut self, id: i32) {
        if let Some(cancel) = self.supervisors.remove(&id) {
            cancel.cancel();
        }
        if let Some(hash) = self.fetch_info_hash(id).await
            && let Err(err) = TorrentEngine::get().pause(&hash).await
        {
            tracing::debug!(?err, id, "Cancel: engine.pause returned error");
        }
        if let Err(err) = sqlx::query!(
            "UPDATE downloads SET status = 'cancelled' WHERE id = $1",
            id
        )
        .execute(&self.db)
        .await
        {
            tracing::error!(?err, id, "Failed to mark download as cancelled");
        }
    }

    async fn handle_remove(&mut self, id: i32) {
        if let Some(cancel) = self.supervisors.remove(&id) {
            cancel.cancel();
        }
        let hash = self.fetch_info_hash(id).await;
        if let Some(h) = hash {
            TorrentEngine::get().stop_and_delete(&h).await;
        }
        if let Err(err) = sqlx::query!("DELETE FROM downloads WHERE id = $1", id)
            .execute(&self.db)
            .await
        {
            tracing::error!(?err, id, "Failed to delete download row");
        }
    }

    async fn handle_refresh(&mut self) {
        let queued = match sqlx::query_scalar!(
            "SELECT id FROM downloads WHERE status = 'queued' ORDER BY created_at ASC"
        )
        .fetch_all(&self.db)
        .await
        {
            Ok(res) => res,
            Err(err) => {
                tracing::error!(?err, "Failed to query queued downloads");
                return;
            }
        };
        for id in queued {
            if self.semaphore.available_permits() == 0 {
                break;
            }
            self.handle_start(id).await;
        }
    }

    async fn handle_done(&mut self, id: i32) {
        self.supervisors.remove(&id);
        self.handle_refresh().await;
    }

    async fn fetch_info_hash(&self, id: i32) -> Option<String> {
        sqlx::query_scalar!("SELECT info_hash FROM downloads WHERE id = $1", id)
            .fetch_optional(&self.db)
            .await
            .ok()
            .flatten()
    }
}

// ── Supervisor ───────────────────────────────────────────────────────────

async fn supervisor(
    db: Pool,
    config: Arc<Config>,
    http: reqwest::Client,
    id: i32,
    cancel: CancellationToken,
) {
    let row = match load_min(&db, id).await {
        Some(res) => res,
        None => {
            tracing::warn!(id, "Supervisor: row not found, exiting");
            return;
        }
    };
    let info_hash = row.info_hash.clone();
    let file_idx = row.file_idx as usize;
    tracing::info!(id, info_hash = %info_hash, file_idx, "Supervisor starting");

    let engine = TorrentEngine::get();
    let handle = match engine.ensure_torrent(&info_hash, &config).await {
        Ok(handle) => handle,
        Err(err) => {
            fail(&db, id, &err).await;
            return;
        }
    };
    if let Err(err) = engine.resume(&info_hash).await {
        tracing::warn!(?err, info_hash, "Supervisor: failed to resume torrent");
    }
    if let Err(err) = engine.select_file(&info_hash, file_idx).await {
        fail(&db, id, &err).await;
        return;
    }

    let stats = handle.managed.stats();
    let name = handle.managed.name();
    if let Err(err) = sqlx::query!(
        "UPDATE downloads SET total_bytes = $1, name = $2 WHERE id = $3",
        stats.total_bytes as i64,
        name.clone(),
        id
    )
    .execute(&db)
    .await
    {
        tracing::warn!(?err, id, "Supervisor: failed to persist total_bytes/name");
    }

    if !row.has_meta
        && config.auto_resolve_stream_metadata
        && let Some(name) = name.clone()
    {
        let db2 = db.clone();
        let http2 = http.clone();
        let config2 = config.clone();
        tokio::spawn(async move {
            resolve_meta_async(db2, config2, http2, id, name).await;
        });
    }

    loop {
        let (downloaded, total) = handle.progress();
        if let Err(err) = sqlx::query!(
            "UPDATE downloads SET downloaded_bytes = $1, total_bytes = $2 WHERE id = $3",
            downloaded as i64,
            total as i64,
            id
        )
        .execute(&db)
        .await
        {
            tracing::warn!(?err, id, "Supervisor: failed to write progress");
        }

        if handle.managed.stats().finished {
            if let Err(err) = sqlx::query!(
                "UPDATE downloads SET status = 'completed', completed_at = CURRENT_TIMESTAMP WHERE id = $1",
                id
            )
            .execute(&db)
            .await
            {
                tracing::error!(?err, id, "Supervisor: failed to mark completed");
            }
            tracing::info!(id, "Download completed");
            return;
        }

        tokio::select! {
            _ = cancel.cancelled() => {
                tracing::debug!(id, "Supervisor cancelled");
                return;
            }
            _ = tokio::time::sleep(Duration::from_secs(3)) => {}
        }
    }
}

async fn fail(db: &Pool, id: i32, err: &crate::app::Error) {
    tracing::error!(id, error = %err, "download failed");
    if let Err(err) = sqlx::query!(
        "UPDATE downloads SET status = 'failed', error = $1 WHERE id = $2",
        err.to_string(),
        id
    )
    .execute(db)
    .await
    {
        tracing::error!(?err, id, "Failed to record failure status");
    }
}

struct MinRow {
    info_hash: String,
    file_idx: i32,
    has_meta: bool,
}

async fn load_min(db: &Pool, id: i32) -> Option<MinRow> {
    let res = sqlx::query!(
        r#"
        SELECT
            d.info_hash,
            d.file_idx,
            EXISTS (SELECT 1 FROM download_meta dm WHERE dm.download_id = d.id) AS "has_meta!"
        FROM downloads d
        WHERE d.id = $1
        "#,
        id
    )
    .fetch_optional(db)
    .await
    .ok()
    .flatten()?;
    Some(MinRow {
        info_hash: res.info_hash,
        file_idx: res.file_idx,
        has_meta: res.has_meta,
    })
}

// ── DB queries ───────────────────────────────────────────────────────────

pub async fn find_all_downloads(db: &Pool) -> crate::app::Result<Vec<Download>> {
    let rows = sqlx::query_as!(
        DownloadRow,
        r#"
        SELECT
            d.id,
            d.info_hash,
            d.file_idx,
            d.name,
            d.total_bytes,
            d.downloaded_bytes,
            d.status as "status: DownloadStatus",
            d.error,
            d.created_at,
            d.completed_at,
            mi.media_type as "media_type?: tmdb::MediaType",
            mi.tmdb_id as "tmdb_id?",
            mi.title as "title?",
            mi.poster_path as "poster_path?",
            dm.season as "season?",
            dm.episode as "episode?",
            dm.resolution as "resolution?"
        FROM downloads d
        LEFT JOIN download_meta dm ON dm.download_id = d.id
        LEFT JOIN media_items mi ON mi.id = dm.media_id
        ORDER BY d.created_at DESC
        "#
    )
    .fetch_all(db)
    .await
    .map_err(crate::app::Error::DatabaseError)?;
    Ok(rows.into_iter().map(Download::from).collect())
}

pub async fn find_download_by_id(id: i32, db: &Pool) -> crate::app::Result<Option<Download>> {
    let row = sqlx::query_as!(
        DownloadRow,
        r#"
        SELECT
            d.id,
            d.info_hash,
            d.file_idx,
            d.name,
            d.total_bytes,
            d.downloaded_bytes,
            d.status as "status: DownloadStatus",
            d.error,
            d.created_at,
            d.completed_at,
            mi.media_type as "media_type?: tmdb::MediaType",
            mi.tmdb_id as "tmdb_id?",
            mi.title as "title?",
            mi.poster_path as "poster_path?",
            dm.season as "season?",
            dm.episode as "episode?",
            dm.resolution as "resolution?"
        FROM downloads d
        LEFT JOIN download_meta dm ON dm.download_id = d.id
        LEFT JOIN media_items mi ON mi.id = dm.media_id
        WHERE d.id = $1
        "#,
        id
    )
    .fetch_optional(db)
    .await
    .map_err(crate::app::Error::DatabaseError)?;
    Ok(row.map(Download::from))
}

// ── Upserts (shared between enqueue, streams.start, async resolver) ─────

#[derive(Clone, Debug)]
pub struct MediaContext {
    pub media_type: tmdb::MediaType,
    pub tmdb_id: i64,
    pub title: String,
    pub poster_path: Option<String>,
    pub season: i32,
    pub episode: i32,
    pub resolution: Option<String>,
}

/// Upsert a `(info_hash, file_idx)` row, returning its id. Does not touch
/// status; the caller decides whether to set/reset it.
pub async fn upsert_download(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    info_hash: &str,
    file_idx: i32,
) -> crate::app::Result<i32> {
    sqlx::query_scalar!(
        r#"
        INSERT INTO downloads (info_hash, file_idx)
        VALUES ($1, $2)
        ON CONFLICT (info_hash, file_idx) DO UPDATE SET info_hash = EXCLUDED.info_hash
        RETURNING id
        "#,
        info_hash,
        file_idx
    )
    .fetch_one(&mut **tx)
    .await
    .map_err(crate::app::Error::DatabaseError)
}

/// Upsert media_items + download_meta linking media context to a download.
pub async fn upsert_meta(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    download_id: i32,
    ctx: &MediaContext,
) -> crate::app::Result<()> {
    let media_id: i32 = sqlx::query_scalar!(
        r#"
        INSERT INTO media_items (media_type, tmdb_id, title, poster_path)
        VALUES ($1, $2, $3, $4)
        ON CONFLICT (media_type, tmdb_id) DO UPDATE SET
            title = EXCLUDED.title,
            poster_path = EXCLUDED.poster_path,
            updated_at = CURRENT_TIMESTAMP
        RETURNING id
        "#,
        ctx.media_type as tmdb::MediaType,
        ctx.tmdb_id,
        ctx.title,
        ctx.poster_path,
    )
    .fetch_one(&mut **tx)
    .await
    .map_err(crate::app::Error::DatabaseError)?;

    sqlx::query!(
        r#"
        INSERT INTO download_meta (download_id, media_id, season, episode, resolution)
        VALUES ($1, $2, $3, $4, $5)
        ON CONFLICT (download_id) DO UPDATE SET
            media_id = EXCLUDED.media_id,
            season = EXCLUDED.season,
            episode = EXCLUDED.episode,
            resolution = EXCLUDED.resolution
        "#,
        download_id,
        media_id,
        ctx.season,
        ctx.episode,
        ctx.resolution,
    )
    .execute(&mut **tx)
    .await
    .map_err(crate::app::Error::DatabaseError)?;
    Ok(())
}

/// Reset a download from a terminal/idle state so it can be re-started.
/// No-op for rows already in flight.
pub async fn reset_for_restart(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    id: i32,
) -> crate::app::Result<()> {
    sqlx::query!(
        r#"
        UPDATE downloads
        SET status = 'queued',
            error = NULL,
            completed_at = NULL
        WHERE id = $1 AND status IN ('paused','cancelled','completed','failed')
        "#,
        id
    )
    .execute(&mut **tx)
    .await
    .map_err(crate::app::Error::DatabaseError)?;
    Ok(())
}

/// Upsert a download row (and optional media context), reset it from any
/// terminal state, and tell the manager to start it. Returns the download id.
/// Used by streams.start (with media context) and the raw stream handler
/// (without).
pub async fn ensure_download(
    db: &Pool,
    handle: &Handle,
    info_hash: &str,
    file_idx: i32,
    media: Option<&MediaContext>,
) -> crate::app::Result<i32> {
    let mut tx = db.begin().await.map_err(crate::app::Error::DatabaseError)?;

    let id = upsert_download(&mut tx, info_hash, file_idx).await?;

    if let Some(ctx) = media {
        upsert_meta(&mut tx, id, ctx).await?;
    }

    reset_for_restart(&mut tx, id).await?;

    tx.commit()
        .await
        .map_err(crate::app::Error::DatabaseError)?;

    handle.send(DownloadCommand::Start(id)).await;

    Ok(id)
}

// ── Async TMDB metadata resolution ───────────────────────────────────────

async fn resolve_meta_async(
    db: Pool,
    config: Arc<Config>,
    http: reqwest::Client,
    id: i32,
    name: String,
) {
    let parsed = parse_release_name(&name);
    let Some(query) = parsed.title.clone() else {
        return;
    };
    let client = TmdbClient::new(&config, http);
    let results = match client.search(&query).await {
        Ok(res) => res,
        Err(err) => {
            tracing::debug!(?err, name, "TMDB search failed");
            return;
        }
    };
    let Some(best) = results.into_iter().next() else {
        tracing::debug!(name, "no TMDB match found");
        return;
    };
    let bt = best.title.to_lowercase();
    let qt = query.to_lowercase();

    if !bt.contains(&qt) && !qt.contains(&bt) {
        tracing::debug!(
            name,
            candidate = %best.title,
            "TMDB match below confidence threshold"
        );
        return;
    }

    let media_ctx = MediaContext {
        media_type: best.media_type,
        tmdb_id: best.id,
        title: best.title,
        poster_path: best.poster_path,
        season: parsed.season.unwrap_or(0),
        episode: parsed.episode.unwrap_or(0),
        resolution: parsed.resolution,
    };

    let mut tx = match db.begin().await {
        Ok(t) => t,
        Err(err) => {
            tracing::warn!(?err, "Async meta upsert: begin tx failed");
            return;
        }
    };
    if let Err(err) = upsert_meta(&mut tx, id, &media_ctx).await {
        tracing::warn!(?err, id, "Async meta upsert failed");
        return;
    }
    if let Err(err) = tx.commit().await {
        tracing::warn!(?err, id, "Async meta upsert commit failed");
        return;
    }

    tracing::info!(
        id,
        name,
        tmdb_id = media_ctx.tmdb_id,
        "Resolved metadata via TMDB"
    );
}

struct ParsedName {
    title: Option<String>,
    season: Option<i32>,
    episode: Option<i32>,
    resolution: Option<String>,
}

/// Best-effort parse of a release-style torrent name. Returns whatever it can
/// extract; missing fields stay None.
fn parse_release_name(raw: &str) -> ParsedName {
    let normalized = raw.replace(['.', '_'], " ");
    let lower = normalized.to_lowercase();

    let resolution = ["2160p", "4k", "1080p", "720p", "480p"]
        .iter()
        .find(|r| lower.contains(*r))
        .map(|r| match *r {
            "2160p" | "4k" => "4K".to_string(),
            other => other.to_string(),
        });

    // SxxExx pattern.
    let (mut season, mut episode) = (None, None);
    let bytes = lower.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b's' {
            let mut j = i + 1;
            let mut s = String::new();
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                s.push(bytes[j] as char);
                j += 1;
            }
            if !s.is_empty() && j < bytes.len() && bytes[j] == b'e' {
                let mut k = j + 1;
                let mut e = String::new();
                while k < bytes.len() && bytes[k].is_ascii_digit() {
                    e.push(bytes[k] as char);
                    k += 1;
                }
                if !e.is_empty() {
                    season = s.parse().ok();
                    episode = e.parse().ok();
                    break;
                }
            }
        }
        i += 1;
    }

    // Title: everything before the first "stop token" (resolution or year).
    let stop_candidates = ["2160p", "4k", "1080p", "720p", "480p"];
    let mut stop_idx = stop_candidates.iter().filter_map(|t| lower.find(t)).min();
    for year in 1900..=2099u32 {
        let y = year.to_string();
        if let Some(idx) = lower.find(&y) {
            stop_idx = Some(stop_idx.map_or(idx, |s| s.min(idx)));
        }
    }
    let stop = stop_idx.unwrap_or(normalized.len());
    let title_raw = normalized[..stop].trim().to_string();
    let title = if title_raw.is_empty() {
        None
    } else {
        Some(title_raw)
    };

    ParsedName {
        title,
        season,
        episode,
        resolution,
    }
}
