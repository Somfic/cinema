use crate::{
    app::{Error, Pool},
    tmdb,
};

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

#[derive(Clone, Debug)]
pub struct MediaContext {
    pub media_type: tmdb::MediaType,
    pub tmdb_id: i64,
    pub title: String,
    pub poster_path: Option<String>,
    pub season: u32,
    pub episode: u32,
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

/// Flat row mapped from the LEFT JOIN'd select; collapsed to [`Download`] via `From`.
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

impl Download {
    pub async fn find_all(db: &Pool) -> crate::app::Result<Vec<Download>> {
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
        .map_err(Error::DatabaseError)?;
        Ok(rows.into_iter().map(Download::from).collect())
    }

    pub async fn find_by_id(id: i32, db: &Pool) -> crate::app::Result<Option<Download>> {
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
        .map_err(Error::DatabaseError)?;
        Ok(row.map(Download::from))
    }

    /// Upsert a `(info_hash, file_idx)` row, returning its id. Does not touch
    /// status; the caller decides whether to set/reset it.
    pub async fn upsert(
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
        .map_err(Error::DatabaseError)
    }

    /// Upsert media_items + download_meta linking media context to a download.
    pub async fn upsert_meta(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        download_id: i32,
        media: &MediaContext,
        ctx: &crate::app::AppContext,
    ) -> crate::app::Result<()> {
        let media_id =
            crate::tmdb::MediaItem::ensure_exists(media.tmdb_id, media.media_type, tx, ctx).await?;

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
            media.season as i32,
            media.episode as i32,
            media.resolution,
        )
        .execute(&mut **tx)
        .await
        .map_err(Error::DatabaseError)?;
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
            error = NULL
        WHERE id = $1 AND status IN ('paused', 'cancelled', 'failed')
        "#,
            id
        )
        .execute(&mut **tx)
        .await
        .map_err(Error::DatabaseError)?;
        Ok(())
    }

    /// Upsert a download row (and optional media context), reset it from any
    /// terminal state, and start it. Blocks until the supervisor is spawned
    /// (or returns a non-`Started` outcome). Returns the download id.
    pub async fn ensure_download(
        ctx: &crate::app::AppContext,
        info_hash: &str,
        file_idx: i32,
        media: Option<&MediaContext>,
    ) -> crate::app::Result<i32> {
        let mut tx = ctx.db.begin().await.map_err(Error::DatabaseError)?;

        let id = Self::upsert(&mut tx, info_hash, file_idx).await?;

        if let Some(media) = media {
            Self::upsert_meta(&mut tx, id, media, ctx).await?;
        }

        Self::reset_for_restart(&mut tx, id).await?;

        tx.commit().await.map_err(Error::DatabaseError)?;

        ctx.downloads.start(id).await?;

        Ok(id)
    }
}
