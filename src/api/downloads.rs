use crate::app::{AppContext, Error};
pub use crate::downloads::Download;
use crate::downloads::DownloadStatus;
use crate::tmdb::TmdbClient;
use crate::{streams as streams_mod, tmdb};

/// Streaming progress for an active download. Emitted periodically by the
/// download worker; subscribers should treat updates as best-effort.
#[draad::ty]
pub struct DownloadProgress {
    pub id: i64,
    pub downloaded_bytes: i64,
    pub total_bytes: Option<i64>,
    pub status: String,
}

#[draad::ty]
pub struct EnqueueDownload {
    pub media_type: tmdb::MediaType,
    pub tmdb_id: i64,
    pub title: String,
    pub poster_path: Option<String>,
    #[serde(default)]
    pub season: i32,
    #[serde(default)]
    pub episode: i32,
    pub resolution: String,
    pub info_hash: Option<String>,
    pub file_idx: Option<i32>,
}

#[draad::ty]
pub struct ResolutionEstimate {
    pub resolution: String,
    pub size_bytes: Option<u64>,
    pub size_display: Option<String>,
    pub streams_count: i64,
}

#[draad::api(namespace = "downloads")]
pub trait DownloadsApi {
    /// Lists every download ever queued, newest first
    async fn list(&self) -> Result<Vec<Download>, Error>;

    /// Cancels an in-progress download and removes its row + files from disk
    async fn delete(&self, id: i32) -> Result<(), Error>;

    /// Queues a download. If `info_hash`/`file_idx` are omitted, picks the
    /// best stream matching the requested resolution
    async fn enqueue(&self, request: EnqueueDownload) -> Result<(), Error>;

    /// Bandwidth/size estimates per available resolution
    async fn estimate(
        &self,
        media_type: String,
        tmdb_id: i64,
    ) -> Result<Vec<ResolutionEstimate>, Error>;
}

#[draad::api]
impl DownloadsApi for AppContext {
    async fn list(&self) -> Result<Vec<Download>, Error> {
        crate::downloads::find_all_downloads(&self.db).await
    }

    async fn delete(&self, id: i32) -> Result<(), Error> {
        let download = crate::downloads::find_download_by_id(id, &self.db).await?;

        if let Some(download) = download {
            if download.status == DownloadStatus::Downloading {
                sqlx::query!(
                    "UPDATE downloads SET status = 'cancelled' WHERE id = $1",
                    id
                )
                .execute(&self.db)
                .await
                .map_err(Error::DatabaseError)?;
            }
            crate::torrent::TorrentEngine::get()
                .stop_and_delete(&download.info_hash)
                .await;
            sqlx::query!("DELETE FROM downloads WHERE id = $1", id)
                .execute(&self.db)
                .await
                .map_err(Error::DatabaseError)?;
        }
        Ok(())
    }

    async fn enqueue(&self, body: EnqueueDownload) -> Result<(), Error> {
        let (info_hash, file_idx) =
            if let (Some(hash), Some(idx)) = (&body.info_hash, body.file_idx) {
                (hash.clone(), idx)
            } else {
                let tmdb = TmdbClient::new(&self.config, self.http.clone());
                let item = tmdb.details(body.media_type, body.tmdb_id).await?;
                let imdb_id = item
                    .imdb_id
                    .ok_or_else(|| Error::Generic("No IMDB ID found".into()))?;
                let path = if body.media_type == tmdb::MediaType::Tv {
                    format!("series/{imdb_id}:{}:{}", body.season, body.episode)
                } else {
                    format!("movie/{imdb_id}")
                };
                let all_streams =
                    streams_mod::aggregate(&self.http, &self.config.stream_sources, &path).await;
                let stream = all_streams
                    .iter()
                    .find(|s| s.resolution.as_deref() == Some(&body.resolution))
                    .or_else(|| all_streams.first())
                    .ok_or_else(|| Error::Generic("No streams found".into()))?;
                (stream.info_hash.clone(), stream.file_idx)
            };

        let file_path = if body.media_type == tmdb::MediaType::Tv {
            format!("tv/{}/s{}e{}.mp4", body.tmdb_id, body.season, body.episode)
        } else {
            format!("movies/{}.mp4", body.tmdb_id)
        };

        sqlx::query!(
            "INSERT INTO downloads (media_type, tmdb_id, title, poster_path, season, episode, resolution, info_hash, file_idx, file_path)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
             ON CONFLICT(media_type, tmdb_id, season, episode) DO UPDATE SET
                info_hash = excluded.info_hash,
                file_idx = excluded.file_idx,
                resolution = excluded.resolution,
                file_path = excluded.file_path,
                status = 'queued',
                error = NULL,
                downloaded_bytes = 0,
                total_bytes = NULL,
                completed_at = NULL",
                body.media_type as tmdb::MediaType,
                body.tmdb_id,
                body.title,
                body.poster_path,
                body.season,
                body.episode,
                body.resolution,
                info_hash,
                file_idx,
                file_path,
        )
        .execute(&self.db)
        .await
        .map_err(Error::DatabaseError)?;

        self.events
            .publish("download:enqueue", serde_json::json!({}));
        Ok(())
    }

    async fn estimate(
        &self,
        media_type: String,
        tmdb_id: i64,
    ) -> Result<Vec<ResolutionEstimate>, Error> {
        let tmdb = TmdbClient::new(&self.config, self.http.clone());
        let mt = crate::api::media::parse_media_type(&media_type)?;
        let item = tmdb.details(mt, tmdb_id).await?;
        let imdb_id = item
            .imdb_id
            .ok_or_else(|| Error::Generic("No IMDB ID found".into()))?;
        let path = if media_type == "tv" {
            format!("series/{imdb_id}:1:1")
        } else {
            format!("movie/{imdb_id}")
        };

        let all_streams =
            streams_mod::aggregate(&self.http, &self.config.stream_sources, &path).await;

        let mut seen =
            std::collections::HashMap::<String, (Option<u64>, Option<String>, i64)>::new();
        for s in &all_streams {
            let Some(res) = s.resolution.clone() else {
                continue;
            };
            let entry = seen.entry(res).or_insert((None, None, 0));
            entry.2 += 1;
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
        estimates.sort_by_key(|e| std::cmp::Reverse(order(&e.resolution)));
        Ok(estimates)
    }
}

#[draad::events(namespace = "downloads")]
pub trait DownloadsEvents {
    /// Per-download bandwidth/status tick. Topic: `downloads_progress`.
    fn progress(payload: DownloadProgress);
}
