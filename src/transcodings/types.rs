use crate::{
    app::{Error, Pool},
    downloads::types::DownloadMeta,
    tmdb,
};

#[draad::ty]
#[derive(sqlx::Type, PartialEq)]
#[sqlx(type_name = "pretranscoding_status", rename_all = "lowercase")]
pub enum PretranscodingStatus {
    Queued,
    Transcoding,
    Paused,
    Completed,
    Cancelled,
    Failed,
}

/// A single pretranscoding job. Joins its parent download so the UI can render
/// title + media context without a second lookup.
#[draad::ty]
pub struct Pretranscoding {
    pub id: i32,
    pub download_id: i32,
    pub download_info_hash: String,
    pub download_file_idx: i32,
    pub audio_index: i32,
    pub only_audio: bool,
    pub name: Option<String>,
    pub transcoded_ms: i64,
    pub total_ms: Option<i64>,
    pub status: PretranscodingStatus,
    pub error: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
    pub meta: Option<DownloadMeta>,
}

#[derive(Debug)]
struct PretranscodingRow {
    id: i32,
    download_id: i32,
    download_info_hash: String,
    download_file_idx: i32,
    audio_index: i32,
    only_audio: bool,
    name: Option<String>,
    transcoded_ms: i64,
    total_ms: Option<i64>,
    status: PretranscodingStatus,
    error: Option<String>,
    created_at: chrono::DateTime<chrono::Utc>,
    completed_at: Option<chrono::DateTime<chrono::Utc>>,

    meta_exists: bool,
    season: Option<i32>,
    episode: Option<i32>,
    resolution: Option<String>,

    media_type: Option<tmdb::MediaType>,
    tmdb_id: Option<i64>,
    title: Option<String>,
    poster_path: Option<String>,
}

impl From<PretranscodingRow> for Pretranscoding {
    fn from(r: PretranscodingRow) -> Self {
        let meta = if r.meta_exists {
            Some(DownloadMeta {
                media_item: r.media_type.map(|media_type| {
                    crate::downloads::types::DownloadMetaMediaItem {
                        media_type,
                        title: r.title.unwrap_or_default(),
                        poster_path: r.poster_path,
                        tmdb_id: r.tmdb_id.unwrap_or(0),
                    }
                }),
                season: r.season,
                episode: r.episode,
                resolution: r.resolution,
            })
        } else {
            None
        };
        Self {
            id: r.id,
            download_id: r.download_id,
            download_info_hash: r.download_info_hash,
            download_file_idx: r.download_file_idx,
            audio_index: r.audio_index,
            only_audio: r.only_audio,
            name: r.name,
            transcoded_ms: r.transcoded_ms,
            total_ms: r.total_ms,
            status: r.status,
            error: r.error,
            created_at: r.created_at,
            completed_at: r.completed_at,
            meta,
        }
    }
}

impl Pretranscoding {
    pub async fn find_all(db: &Pool) -> crate::app::Result<Vec<Self>> {
        let rows = sqlx::query_as!(
            PretranscodingRow,
            r#"
                SELECT
                    pt.id,
                    d.id as "download_id",
                    d.info_hash as "download_info_hash",
                    d.file_idx as "download_file_idx",
                    pt.audio_index,
                    pt.only_audio,
                    d.name,
                    pt.transcoded_ms,
                    pt.total_ms,
                    pt.status as "status: PretranscodingStatus",
                    pt.error,
                    pt.created_at,
                    pt.completed_at,
                    dm.info_hash IS NOT NULL as "meta_exists!: bool",
                    mi.media_type as "media_type?: tmdb::MediaType",
                    mi.tmdb_id as "tmdb_id?",
                    mi.title as "title?",
                    mi.poster_path as "poster_path?",
                    dm.season as "season?",
                    dm.episode as "episode?",
                    dm.resolution as "resolution?"
                FROM pretranscodings pt
                JOIN downloads d ON d.id = pt.download_id
                LEFT JOIN download_meta dm ON dm.info_hash = d.info_hash AND dm.file_idx = d.file_idx
                LEFT JOIN media_items mi ON mi.id = dm.media_id
                ORDER BY pt.created_at DESC
            "#
        )
        .fetch_all(db)
        .await
        .map_err(Error::DatabaseError)?;
        Ok(rows.into_iter().map(Self::from).collect())
    }
}

pub struct CompletedPretranscoding {
    pub id: i32,
    pub download_id: i32,
}

impl CompletedPretranscoding {
    /// Look up a completed pretranscoding whose cached MP4 the streams.remux
    /// endpoint can reuse. Exact match on all four keys: a full transcode
    /// does not satisfy an only-audio request or vice versa.
    pub async fn find(
        db: &Pool,
        info_hash: &str,
        file_idx: i32,
        only_audio: bool,
        audio_index: i32,
    ) -> crate::app::Result<Option<Self>> {
        sqlx::query_as!(
            Self,
            r#"
                SELECT
                    pt.id,
                    pt.download_id
                FROM pretranscodings pt
                    JOIN downloads d ON d.id = pt.download_id
                WHERE d.info_hash = $1
                    AND d.file_idx = $2
                    AND pt.only_audio = $3
                    AND pt.audio_index = $4
                    AND pt.status = 'completed'
                LIMIT 1
            "#,
            info_hash,
            file_idx,
            only_audio,
            audio_index,
        )
        .fetch_optional(db)
        .await
        .map_err(Error::DatabaseError)
    }
}
