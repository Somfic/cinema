use crate::{
    app::{AppContext, Error},
    tmdb,
};

#[draad::ty]
pub struct RecordWatch {
    pub media_type: tmdb::MediaType,
    pub tmdb_id: i64,
    pub title: String,
    pub poster_path: Option<String>,
    pub season: Option<i32>,
    pub episode: Option<i32>,
    pub info_hash: Option<String>,
    pub file_idx: Option<i32>,
    pub progress: Option<f32>,
    pub duration: Option<f32>,
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
    pub last_watched: chrono::DateTime<chrono::Utc>,
}

#[draad::api(namespace = "watch")]
pub trait WatchApi {
    /// Inserts the current playback position for a piece of media
    async fn record(&self, watch: RecordWatch) -> Result<(), Error>;

    /// Returns the 20 most-recently-watched items
    async fn history(&self) -> Result<Vec<WatchHistoryItem>, Error>;
}

#[draad::api]
impl WatchApi for AppContext {
    async fn record(&self, watch: RecordWatch) -> Result<(), Error> {
        sqlx::query!(
            "INSERT INTO watch_history (media_type, tmdb_id, title, poster_path, season, episode, info_hash, file_idx, progress, duration)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
             ON CONFLICT(media_type, tmdb_id)
             DO UPDATE SET
                title = excluded.title,
                poster_path = excluded.poster_path,
                season = excluded.season,
                episode = excluded.episode,
                info_hash = excluded.info_hash,
                file_idx = excluded.file_idx,
                progress = excluded.progress,
                duration = excluded.duration,
                last_watched = CURRENT_TIMESTAMP",
            watch.media_type as tmdb::MediaType,
            watch.tmdb_id,
            watch.title,
            watch.poster_path,
            watch.season,
            watch.episode,
            watch.info_hash,
            watch.file_idx,
            watch.progress,
            watch.duration,
    )
        .execute(&self.db)
        .await
        .map_err(Error::DatabaseError)?;

        Ok(())
    }

    async fn history(&self) -> Result<Vec<WatchHistoryItem>, Error> {
        let items = sqlx::query_as!(
            WatchHistoryItem,
            r#"SELECT
                media_type as "media_type: tmdb::MediaType",
                tmdb_id,
                title,
                poster_path,
                season,
                episode,
                info_hash,
                file_idx,
                progress,
                duration,
                last_watched
            FROM watch_history
            ORDER BY last_watched DESC
            LIMIT 20"#
        )
        .fetch_all(&self.db)
        .await
        .map_err(Error::DatabaseError)?;

        Ok(items)
    }
}
