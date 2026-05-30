use std::sync::Arc;

use tokio::sync::Semaphore;

use crate::app::AppContext;
use crate::torrent::TorrentEngine;

#[draad::ty]
#[derive(sqlx::FromRow)]
pub struct Download {
    pub id: i64,
    pub media_type: String,
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
    pub status: String,
    pub error: Option<String>,
    pub created_at: String,
    pub completed_at: Option<String>,
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
            sqlx::query("UPDATE downloads SET status = 'queued' WHERE status = 'downloading'")
                .execute(&self.ctx.db)
                .await;
        if let Ok(r) = &reset {
            if r.rows_affected() > 0 {
                tracing::info!(
                    count = r.rows_affected(),
                    "Reset interrupted downloads to queued"
                );
            }
        }
        tracing::info!("Download manager started");

        let mut rx = self.ctx.events.subscribe();

        loop {
            // Fetch next queued download
            let queued = sqlx::query_as::<_, Download>(
                "SELECT * FROM downloads WHERE status = 'queued' ORDER BY created_at ASC LIMIT 1",
            )
            .fetch_optional(&self.ctx.db)
            .await;

            if let Ok(Some(download)) = queued {
                let permit = self.semaphore.clone().acquire_owned().await;
                if let Ok(permit) = permit {
                    let ctx = self.ctx.clone();
                    tokio::spawn(async move {
                        download_file(ctx, download).await;
                        drop(permit);
                    });
                    continue;
                }
            }

            // Wait for a wake event or poll every 10 seconds
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_secs(10)) => {}
                msg = rx.recv() => {
                    if let Ok(event) = msg {
                        if event.topic == "download:enqueue" {
                            continue;
                        }
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

    let _ = sqlx::query("UPDATE downloads SET status = 'downloading' WHERE id = ?")
        .bind(id)
        .execute(&ctx.db)
        .await;

    match do_download(&ctx, &download).await {
        Ok(()) => {
            tracing::info!(id, title = download.title, "Download completed");
            let _ = sqlx::query(
                "UPDATE downloads SET status = 'completed', completed_at = datetime('now') WHERE id = ?",
            )
            .bind(id)
            .execute(&ctx.db)
            .await;
        }
        Err(e) => {
            tracing::error!(id, title = download.title, error = %e, "Download failed");
            let _ = sqlx::query("UPDATE downloads SET status = 'failed', error = ? WHERE id = ?")
                .bind(e.to_string())
                .bind(id)
                .execute(&ctx.db)
                .await;
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

        let _ =
            sqlx::query("UPDATE downloads SET downloaded_bytes = ?, total_bytes = ? WHERE id = ?")
                .bind(downloaded as i64)
                .bind(total as i64)
                .bind(download.id)
                .execute(&ctx.db)
                .await;

        // Check cancellation
        let status: Option<(String,)> = sqlx::query_as("SELECT status FROM downloads WHERE id = ?")
            .bind(download.id)
            .fetch_optional(&ctx.db)
            .await
            .unwrap_or(None);

        if let Some((s,)) = status
            && s == "cancelled"
        {
            engine.stop(&download.info_hash).await;
            return Err(crate::app::Error::Generic("Download cancelled".into()));
        }

        let stats = handle.managed.stats();
        if stats.finished {
            break;
        }

        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    }

    Ok(())
}
