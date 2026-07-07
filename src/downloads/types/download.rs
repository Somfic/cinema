use std::collections::HashMap;

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
pub struct SimpleDownload {
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
}

#[draad::ty]
pub struct Download {
    pub id: i32,
    pub info_hash: String,
    pub file_idx: i32,
    pub name: Option<String>,
    pub total_bytes: Option<i64>,
    pub downloaded_bytes: i64,
    pub status: DownloadStatus,
    pub error: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
    pub meta: Option<super::DownloadMeta>,
}

/// Flat row mapped from the LEFT JOIN'd select; collapsed to [`Download`] via `From`.
#[derive(Debug)]
struct DownloadRow {
    // download itself
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

    // download meta
    meta_exists: bool,
    season: Option<i32>,
    episode: Option<i32>,
    resolution: Option<String>,

    // download media item
    media_type: Option<tmdb::MediaType>,
    tmdb_id: Option<i64>,
    title: Option<String>,
    poster_path: Option<String>,
}

impl From<DownloadRow> for Download {
    fn from(r: DownloadRow) -> Self {
        let meta = if r.meta_exists {
            Some(super::DownloadMeta {
                media_item: r.media_type.map(|media_type| super::DownloadMetaMediaItem {
                    media_type,
                    title: r.title.unwrap_or_default(),
                    poster_path: r.poster_path,
                    tmdb_id: r.tmdb_id.unwrap_or(0),
                }),
                season: r.season,
                episode: r.episode,
                resolution: r.resolution,
            })
        } else {
            None
        };

        Download {
            id: r.id,
            info_hash: r.info_hash,
            file_idx: r.file_idx,
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
    pub async fn find_all(db: &Pool) -> crate::app::Result<Vec<Self>> {
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
                    dm.info_hash IS NOT NULL as "meta_exists!: bool",
                    mi.media_type as "media_type?: tmdb::MediaType",
                    mi.tmdb_id as "tmdb_id?",
                    mi.title as "title?",
                    mi.poster_path as "poster_path?",
                    dm.season as "season?",
                    dm.episode as "episode?",
                    dm.resolution as "resolution?"
                FROM downloads d
                LEFT JOIN download_meta dm ON dm.info_hash = d.info_hash AND dm.file_idx = d.file_idx
                LEFT JOIN media_items mi ON mi.id = dm.media_id
                ORDER BY d.created_at DESC
            "#
        )
        .fetch_all(db)
        .await?;

        Ok(rows.into_iter().map(Self::from).collect())
    }

    pub async fn find_id_by_info_hash_and_file_idx(
        db: &Pool,
        info_hash: &str,
        file_idx: i32,
    ) -> crate::app::Result<Option<i32>> {
        let id = sqlx::query_scalar!(
            r#"
                SELECT id FROM downloads WHERE info_hash = $1 AND file_idx = $2
            "#,
            info_hash,
            file_idx
        )
        .fetch_optional(db)
        .await?;

        Ok(id)
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
        .await?;

        Ok(())
    }
}

impl SimpleDownload {
    pub async fn find_by_info_hash_and_file_idx(
        db: &Pool,
        keys: Vec<(&str, i32)>,
    ) -> crate::app::Result<HashMap<(String, i32), Self>> {
        let (info_hashes, file_indexes): (Vec<_>, Vec<_>) = keys.into_iter().unzip();

        let rows = sqlx::query_as!(
            SimpleDownload,
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
                    d.completed_at
                FROM downloads d
                WHERE (d.info_hash, d.file_idx) IN (
                    SELECT * FROM UNNEST($1::text[], $2::int4[])
                )
            "#,
            &info_hashes as &[&str],
            &file_indexes
        )
        .fetch_all(db)
        .await?;

        let map = rows
            .into_iter()
            .map(|val| ((val.info_hash.clone(), val.file_idx), val))
            .collect();

        Ok(map)
    }
}
