use crate::{
    app::{AppContext, CinemaError},
    tmdb,
};

#[draad::ty]
pub struct RecordWatch {
    pub tmdb_id: i64,
    pub media_type: tmdb::MediaType,

    pub info_hash: Option<String>,
    pub file_idx: Option<i32>,

    pub season: Option<i32>,
    pub episode: Option<i32>,
    pub progress: Option<f32>,
    pub duration: Option<f32>,
    pub transcoding: TranscodingOption,
}

#[draad::ty]
#[derive(sqlx::Type)]
#[sqlx(type_name = "transcoding_option", rename_all = "kebab-case")]
pub enum TranscodingOption {
    Enabled,
    OnlyAudio,
    Disabled,
}

#[draad::ty]
pub struct WatchHistoryItem {
    pub media_type: tmdb::MediaType,
    pub tmdb_id: i64,
    pub title: String,
    pub poster_path: Option<String>,
    pub season: i64,
    pub episode: i64,
    pub info_hash: Option<String>,
    pub file_idx: i64,
    pub progress: f64,
    pub duration: f64,
    pub transcoding: TranscodingOption,
    pub last_watched: chrono::DateTime<chrono::Utc>,
}

#[draad::api(namespace = "watch")]
pub trait WatchApi {
    /// Inserts the current playback position for a piece of media
    #[post]
    async fn record(&self, watch: RecordWatch) -> Result<(), CinemaError>;

    /// Returns the 20 most-recently-watched items
    #[get]
    async fn history(&self) -> Result<Vec<WatchHistoryItem>, CinemaError>;
}

#[draad::api]
impl WatchApi for AppContext {
    async fn record(&self, watch: RecordWatch) -> Result<(), CinemaError> {
        let mut tx = self.db.begin().await.map_err(CinemaError::DatabaseError)?;

        let media_id =
            crate::tmdb::MediaItem::ensure_exists(watch.tmdb_id, watch.media_type, &mut tx, self)
                .await?;

        // Best-effort link to the download row used as the playback source.
        let download_id = if let Some(hash) = watch.info_hash.as_deref() {
            sqlx::query_scalar!(
                "SELECT id FROM downloads WHERE info_hash = $1 AND file_idx = $2 LIMIT 1",
                hash,
                watch.file_idx.unwrap_or(0),
            )
            .fetch_optional(&mut *tx)
            .await
            .map_err(CinemaError::DatabaseError)?
        } else {
            None
        };

        sqlx::query!(
            "
                INSERT INTO watch_history
                    (media_id, download_id, season, episode, progress, duration, transcoding)
                VALUES ($1, $2, $3, $4, $5, $6, $7)
                ON CONFLICT (media_id) DO UPDATE SET
                    download_id  = EXCLUDED.download_id,
                    season       = EXCLUDED.season,
                    episode      = EXCLUDED.episode,
                    progress     = EXCLUDED.progress,
                    duration     = EXCLUDED.duration,
                    transcoding  = EXCLUDED.transcoding,
                    last_watched = CURRENT_TIMESTAMP
            ",
            media_id,
            download_id,
            watch.season.unwrap_or(0),
            watch.episode.unwrap_or(0),
            watch.progress.unwrap_or(0.0),
            watch.duration.unwrap_or(0.0),
            watch.transcoding as TranscodingOption,
        )
        .execute(&mut *tx)
        .await
        .map_err(CinemaError::DatabaseError)?;

        tx.commit().await.map_err(CinemaError::DatabaseError)?;
        Ok(())
    }

    async fn history(&self) -> Result<Vec<WatchHistoryItem>, CinemaError> {
        let items = sqlx::query_as!(
            WatchHistoryItem,
            r#"SELECT
                mi.media_type as "media_type: tmdb::MediaType",
                mi.tmdb_id,
                mi.title,
                mi.poster_path,
                wh.season,
                wh.episode,
                d.info_hash as "info_hash?",
                COALESCE(d.file_idx, 0) as "file_idx!",
                wh.progress,
                wh.duration,
                wh.transcoding as "transcoding: TranscodingOption",
                wh.last_watched
            FROM watch_history wh
            JOIN media_items mi ON mi.id = wh.media_id
            LEFT JOIN downloads d ON d.id = wh.download_id
            ORDER BY wh.last_watched DESC
            LIMIT 20"#
        )
        .fetch_all(&self.db)
        .await
        .map_err(CinemaError::DatabaseError)?;

        Ok(items)
    }
}
