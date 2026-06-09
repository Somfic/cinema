use crate::app::{AppContext, Error};
pub use crate::downloads::Download;
use crate::downloads::{DownloadCommand, MediaContext};
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

    /// Queue a new download. If `info_hash`/`file_idx` are omitted, picks the
    /// best stream matching the requested resolution. Returns the download id
    async fn enqueue(&self, request: EnqueueDownload) -> Result<i32, Error>;

    /// Temporarily pause. Files stay on disk; resume picks up where it left off.
    async fn pause(&self, id: i32) -> Result<(), Error>;

    /// Resume a paused or cancelled download. Also re-runs a failed download
    async fn resume(&self, id: i32) -> Result<(), Error>;

    /// Stop the download but keep the files. Distinct from `pause` in intent
    /// (user no longer wants this download).
    async fn cancel(&self, id: i32) -> Result<(), Error>;

    /// Stop and wipe. Removes the row and deletes files from disk
    async fn remove(&self, id: i32) -> Result<(), Error>;

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

    async fn enqueue(&self, body: EnqueueDownload) -> Result<i32, Error> {
        let (info_hash, file_idx) =
            if let (Some(hash), Some(idx)) = (&body.info_hash, body.file_idx) {
                (hash.clone(), idx)
            } else {
                // TODO: rewrite to remove this part - should work like /streams/start,
                //  with only info_hash and an optional metadata
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

        let media = MediaContext {
            media_type: body.media_type,
            tmdb_id: body.tmdb_id,
            title: body.title,
            poster_path: body.poster_path,
            season: body.season,
            episode: body.episode,
            resolution: Some(body.resolution),
        };

        let id = crate::downloads::ensure_download(
            &self.db,
            &self.downloads,
            &info_hash,
            file_idx,
            Some(&media),
        )
        .await?;
        Ok(id)
    }

    async fn pause(&self, id: i32) -> Result<(), Error> {
        self.downloads.send(DownloadCommand::Pause(id)).await;
        Ok(())
    }

    async fn resume(&self, id: i32) -> Result<(), Error> {
        // Reset terminal/idle state so the manager will pick it up as queued.
        let mut tx = self.db.begin().await.map_err(Error::DatabaseError)?;
        crate::downloads::reset_for_restart(&mut tx, id).await?;
        tx.commit().await.map_err(Error::DatabaseError)?;
        self.downloads.send(DownloadCommand::Start(id)).await;
        Ok(())
    }

    async fn cancel(&self, id: i32) -> Result<(), Error> {
        self.downloads.send(DownloadCommand::Cancel(id)).await;
        Ok(())
    }

    async fn remove(&self, id: i32) -> Result<(), Error> {
        self.downloads.send(DownloadCommand::Remove(id)).await;
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
