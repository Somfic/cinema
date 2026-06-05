use std::sync::Arc;

use tokio::sync::Semaphore;

use crate::app::AppContext;
use crate::tmdb;
use crate::torrent::TorrentEngine;

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
#[derive(sqlx::FromRow)]
pub struct Download {
    pub id: i32,
    pub media_type: tmdb::MediaType,
    pub tmdb_id: i64,
    pub title: String,
    pub poster_path: Option<String>,
    pub season: i64,
    pub episode: i64,
    pub resolution: Option<String>,
    pub info_hash: String,
    pub file_idx: i64,
    pub file_path: String,
    pub total_bytes: Option<i64>,
    pub downloaded_bytes: i64,
    pub status: DownloadStatus,
    pub error: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
}

pub struct DownloadManager {
    ctx: AppContext,
    semaphore: Arc<Semaphore>,
}

impl DownloadManager {
    pub fn new(ctx: AppContext) -> Self {
        let permits = ctx.config.max_concurrent_downloads;
        Self {
            ctx,
            semaphore: Arc::new(Semaphore::new(permits)),
        }
    }

    pub async fn run(self) {
        // Reset interrupted downloads on startup
        let reset =
            sqlx::query!("UPDATE downloads SET status = 'queued' WHERE status = 'downloading'")
                .execute(&self.ctx.db)
                .await;
        match reset {
            Ok(res) => {
                if res.rows_affected() > 0 {
                    tracing::info!(
                        count = res.rows_affected(),
                        "Reset interrupted downloads to queued"
                    );
                }
            }
            Err(err) => {
                tracing::error!(?err, "Error while resetting interrupted downloads");
            }
        }
        tracing::info!("Download manager started");

        let mut rx = self.ctx.events.subscribe();

        loop {
            // Fetch next queued download
            let queued = sqlx::query_as!(
                Download,
                r#"
                SELECT
                    id,
                    media_type as "media_type: tmdb::MediaType",
                    tmdb_id,
                    title,
                    poster_path,
                    season,
                    episode,
                    resolution,
                    info_hash,
                    file_idx,
                    file_path,
                    total_bytes,
                    downloaded_bytes,
                    status as "status: DownloadStatus",
                    error,
                    created_at,
                    completed_at
                FROM downloads
                WHERE status = 'queued'
                ORDER BY created_at ASC
                LIMIT 1
                "#,
            )
            .fetch_optional(&self.ctx.db)
            .await;

            match queued {
                Ok(Some(download)) => {
                    let permit = self.semaphore.clone().acquire_owned().await;
                    match permit {
                        Ok(permit) => {
                            let ctx = self.ctx.clone();
                            tokio::spawn(async move {
                                download_file(ctx, download).await;
                                drop(permit);
                            });
                            continue;
                        }
                        Err(err) => {
                            tracing::warn!(
                                ?err,
                                ?download,
                                "Could not acquire the permit to start downloading, skipping"
                            );
                        }
                    }
                }
                Ok(_) => (),
                Err(err) => {
                    tracing::error!(?err, "Error while fetching the next queued download");
                }
            }

            // Wait for a wake event or poll every 10 seconds
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_secs(10)) => {}
                msg = rx.recv() => {
                    if let Ok(event) = msg && event.topic == "download:enqueue" {
                            continue;
                    }
                }
            }
        }
    }
}

async fn download_file(ctx: AppContext, download: Download) {
    let id = download.id;
    tracing::info!(
        id,
        title = download.title,
        file = download.file_path,
        "Starting download"
    );

    if let Err(err) = sqlx::query!(
        "UPDATE downloads SET status = 'downloading' WHERE id = $1",
        id
    )
    .execute(&ctx.db)
    .await
    {
        tracing::error!(
            ?err,
            ?download,
            "Error setting the status to \"downloading\", aborting"
        );
        return;
    }

    match do_download(&ctx, &download).await {
        Ok(()) => {
            tracing::info!(id, title = download.title, "Download completed");
            if let Err(err) = sqlx::query!(
                "UPDATE downloads SET status = 'completed', completed_at = CURRENT_TIMESTAMP WHERE id = $1",
                id
            )
            .execute(&ctx.db)
                .await
            {
                tracing::error!(
                    ?err,
                    ?download,
                    "Error setting the status to \"completed\", the database is now probably out of sync!"
                );
            }
        }
        Err(e) => {
            tracing::error!(id, title = download.title, error = %e, "Download failed");
            if let Err(err) = sqlx::query!(
                "UPDATE downloads SET status = 'failed', error = $1 WHERE id = $2",
                e.to_string(),
                id
            )
            .execute(&ctx.db)
            .await
            {
                tracing::error!(
                    ?err,
                    ?download,
                    "Error setting the status to \"failed\", the database is now probably out of sync!"
                );
            }
        }
    }
}

async fn do_download(ctx: &AppContext, download: &Download) -> crate::app::Result<()> {
    let engine = TorrentEngine::get();
    let handle = engine
        .start(&download.info_hash, download.file_idx as usize, &ctx.config)
        .await?;

    // Poll progress until complete
    loop {
        let (downloaded, total) = handle.progress();

        if let Err(err) = sqlx::query!(
            "UPDATE downloads SET downloaded_bytes = $1, total_bytes = $2 WHERE id = $3",
            downloaded as i64,
            total as i64,
            download.id
        )
        .execute(&ctx.db)
        .await
        {
            tracing::error!(?err, ?download, "Error updating download stats, continuing");
            continue;
        }

        // Check cancellation
        let check = sqlx::query!(
            "SELECT 1 as one FROM downloads WHERE id = $1 AND status = 'cancelled'",
            download.id
        )
        .fetch_optional(&ctx.db)
        .await;
        match check {
            Ok(res) => {
                if res.is_some() {
                    // The query returned a result, so the status must be cancelled
                    engine.stop(&download.info_hash).await;
                    return Err(crate::app::Error::Generic("Download cancelled".into()));
                }
            }
            Err(err) => {
                tracing::error!(
                    ?err,
                    ?download,
                    "Error checking for cancelled status for download, continuing"
                );
            }
        }

        let stats = handle.managed.stats();
        if stats.finished {
            break;
        }

        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    }

    Ok(())
}

pub async fn find_all_downloads(db: &crate::app::Pool) -> crate::app::Result<Vec<Download>> {
    sqlx::query_as!(
        Download,
        r#"
            SELECT
                id,
                media_type as "media_type: tmdb::MediaType",
                tmdb_id,
                title,
                poster_path,
                season,
                episode,
                resolution,
                info_hash,
                file_idx,
                file_path,
                total_bytes,
                downloaded_bytes,
                status as "status: DownloadStatus",
                error,
                created_at,
                completed_at
            FROM downloads
            ORDER BY created_at DESC
        "#,
    )
    .fetch_all(db)
    .await
    .map_err(crate::app::Error::DatabaseError)
}

pub async fn find_download_by_id(
    id: i32,
    db: &crate::app::Pool,
) -> crate::app::Result<Option<Download>> {
    sqlx::query_as!(
        Download,
        r#"
            SELECT
                id,
                media_type as "media_type: tmdb::MediaType",
                tmdb_id,
                title,
                poster_path,
                season,
                episode,
                resolution,
                info_hash,
                file_idx,
                file_path,
                total_bytes,
                downloaded_bytes,
                status as "status: DownloadStatus",
                error,
                created_at,
                completed_at
            FROM downloads
            WHERE id = $1
        "#,
        id,
    )
    .fetch_optional(db)
    .await
    .map_err(crate::app::Error::DatabaseError)
}
