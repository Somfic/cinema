use crate::app::{AppContext, Error};
use crate::downloads::DownloadProgress;
use crate::downloads::types::Download;
use crate::streams as streams_mod;
use crate::tmdb::{MediaType, TmdbClient};

#[draad::ty]
pub struct EnqueueDownload {
    pub info_hash: String,
    pub file_idx: i32,
}

#[draad::ty]
pub struct ResolutionEstimate {
    pub resolution: String,
    pub size_bytes: Option<u64>,
    pub size_display: Option<String>,
    pub streams_count: i64,
}

#[draad::ty]
pub struct DownloadStatusUpdate {
    pub download_id: i32,
    pub new_status: crate::downloads::types::DownloadStatus,
}

#[draad::api(namespace = "downloads")]
pub trait DownloadsApi {
    /// Lists every download ever queued, newest first
    #[get]
    async fn list(&self) -> Result<Vec<Download>, Error>;

    /// Queue a new download. If `info_hash`/`file_idx` are omitted, picks the
    /// best stream matching the requested resolution. Returns the download id
    async fn enqueue(&self, request: EnqueueDownload) -> Result<i32, Error>;

    /// Temporarily pause. Files stay on disk; resume picks up where it left off.
    #[delete]
    async fn pause(&self, id: i32) -> Result<(), Error>;

    /// Resume a paused or cancelled download. Also re-runs a failed download
    async fn resume(&self, id: i32) -> Result<(), Error>;

    /// Stop the download but keep the files. Distinct from `pause` in intent
    /// (user no longer wants this download).
    async fn cancel(&self, id: i32) -> Result<(), Error>;

    /// Stop and wipe. Removes the row and deletes files from disk
    #[post]
    async fn remove(&self, id: i32) -> Result<(), Error>;

    /// Bandwidth/size estimates per available resolution
    #[get]
    async fn estimate(
        &self,
        media_type: String,
        tmdb_id: i64,
    ) -> Result<Vec<ResolutionEstimate>, Error>;
}

#[draad::api]
impl DownloadsApi for AppContext {
    async fn list(&self) -> Result<Vec<Download>, Error> {
        crate::downloads::types::Download::find_all(&self.db).await
    }

    async fn enqueue(&self, body: EnqueueDownload) -> Result<i32, Error> {
        let (id, _) = self
            .downloads
            .ensure_download(
                &body.info_hash,
                body.file_idx,
                crate::downloads::DownloadPriority::Background,
            )
            .await?;

        Ok(id)
    }

    async fn pause(&self, id: i32) -> Result<(), Error> {
        self.downloads.pause(id).await
    }

    async fn resume(&self, id: i32) -> Result<(), Error> {
        // Reset terminal/idle state so start treats it as a fresh launch.
        let mut tx = self.db.begin().await.map_err(Error::DatabaseError)?;
        crate::downloads::types::Download::reset_for_restart(&mut tx, id).await?;
        tx.commit().await.map_err(Error::DatabaseError)?;
        self.downloads
            .start(id, crate::downloads::DownloadPriority::Background)
            .await?;
        Ok(())
    }

    async fn cancel(&self, id: i32) -> Result<(), Error> {
        self.downloads.cancel(id).await
    }

    async fn remove(&self, id: i32) -> Result<(), Error> {
        self.pretranscodings.remove_all_for_download(id).await?;
        self.downloads.remove(id).await
    }

    async fn estimate(
        &self,
        media_type: String,
        tmdb_id: i64,
    ) -> Result<Vec<ResolutionEstimate>, Error> {
        let tmdb = TmdbClient::new(&self.config, self.http.clone());
        let mt = MediaType::try_from(media_type)?;
        let item = tmdb.details(mt, tmdb_id, &self.db).await?;
        let imdb_id = item
            .imdb_id
            .ok_or_else(|| Error::Generic("No IMDB ID found".into()))?;
        let media_type = match mt {
            MediaType::Movie => streams_mod::AggregationMediaType::Media { tmdb_id, imdb_id },
            MediaType::Tv => streams_mod::AggregationMediaType::Tv {
                tmdb_id,
                imdb_id,
                season: 1,
                episode: 1,
            },
        };

        let all_streams = media_type.aggregate(self).await;

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

    fn status_update(payload: DownloadStatusUpdate);

    fn removed(id: i32);
}
