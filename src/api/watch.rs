use cinema_schema::{cinema_api, cinema_type};

use crate::app::{AppContext, Error};

#[cinema_type]
pub struct RecordWatch {
    pub media_type: String,
    pub tmdb_id: i64,
    pub title: String,
    pub poster_path: Option<String>,
    pub season: Option<i64>,
    pub episode: Option<i64>,
    pub info_hash: Option<String>,
    pub file_idx: Option<i64>,
    pub progress: Option<f64>,
    pub duration: Option<f64>,
}

#[cinema_type]
#[derive(sqlx::FromRow)]
pub struct WatchHistoryItem {
    pub media_type: String,
    pub tmdb_id: i64,
    pub title: String,
    pub poster_path: Option<String>,
    pub season: i64,
    pub episode: i64,
    pub info_hash: Option<String>,
    pub file_idx: i64,
    pub progress: f64,
    pub duration: f64,
    pub last_watched: String,
}

#[cinema_api(namespace = "watch")]
pub trait WatchApi {
    /// Inserts the current playback position for a piece of media
    async fn record(&self, watch: RecordWatch) -> Result<(), Error>;

    /// Returns the 20 most-recently-watched items
    async fn history(&self) -> Result<Vec<WatchHistoryItem>, Error>;
}

#[cinema_api]
impl WatchApi for AppContext {
    async fn record(&self, watch: RecordWatch) -> Result<(), Error> {
        sqlx::query(
            "INSERT INTO watch_history (media_type, tmdb_id, title, poster_path, season, episode, info_hash, file_idx, progress, duration)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(media_type, tmdb_id)
             DO UPDATE SET title = excluded.title, poster_path = excluded.poster_path, season = excluded.season, episode = excluded.episode, info_hash = excluded.info_hash, file_idx = excluded.file_idx, progress = excluded.progress, duration = excluded.duration, last_watched = datetime('now')"
        )
        .bind(&watch.media_type)
        .bind(watch.tmdb_id)
        .bind(&watch.title)
        .bind(&watch.poster_path)
        .bind(watch.season.unwrap_or(0))
        .bind(watch.episode.unwrap_or(0))
        .bind(&watch.info_hash)
        .bind(watch.file_idx.unwrap_or(0))
        .bind(watch.progress.unwrap_or(0.0))
        .bind(watch.duration.unwrap_or(0.0))
        .execute(&self.db)
        .await
        .map_err(|e| Error::Generic(e.to_string()))?;
        Ok(())
    }

    async fn history(&self) -> Result<Vec<WatchHistoryItem>, Error> {
        let items = sqlx::query_as::<_, WatchHistoryItem>(
            "SELECT media_type, tmdb_id, title, poster_path, season, episode, info_hash, file_idx, progress, duration, last_watched
             FROM watch_history ORDER BY last_watched DESC LIMIT 20"
        )
        .fetch_all(&self.db)
        .await
        .map_err(|e| Error::Generic(e.to_string()))?;
        Ok(items)
    }
}
