use std::time::Duration;

use crate::app::Pool;

pub struct Supervisor {
    download_id: i32,
    db: Pool,
    handle: super::TorrentHandle,
    cancel: tokio_util::sync::CancellationToken,
}

impl Supervisor {
    pub async fn new(
        db: Pool,
        download_id: i32,
        handle: super::TorrentHandle,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Self {
        let stats = handle.managed.stats();
        let name = handle.managed.name();
        if let Err(err) = sqlx::query!(
            "UPDATE downloads SET total_bytes = $1, name = $2 WHERE id = $3",
            stats.total_bytes as i64,
            name,
            download_id
        )
        .execute(&db)
        .await
        {
            tracing::warn!(
                ?err,
                download_id,
                "Supervisor: failed to persist total_bytes/name"
            );
        }

        tracing::debug!("Supervisor created for download #{download_id}");

        Self {
            download_id,
            db,
            handle,
            cancel,
        }
    }

    pub async fn run(&self) {
        if self.cancel.is_cancelled() {
            // Simple race condition guard: if the the download is cancelled before the supervisor
            // was able to start, just exit immediately
            return;
        }

        tracing::info!("Supervisor started for download #{}", self.download_id);

        if let Err(err) = sqlx::query!(
            "UPDATE downloads SET status = 'downloading', error = NULL WHERE id = $1",
            self.download_id
        )
        .execute(&self.db)
        .await
        {
            tracing::error!(
                ?err,
                self.download_id,
                "Supervisor: failed to set status to downloading"
            );
            return;
        }

        let mut consecutive_failures: u8 = 0;
        const MAX_CONSECUTIVE_FAILURES: u8 = 30;

        loop {
            let (downloaded, total) = self.handle.progress();
            if let Err(err) = sqlx::query!(
                "UPDATE downloads SET downloaded_bytes = $1, total_bytes = $2 WHERE id = $3 AND status = 'downloading'",
                downloaded as i64,
                total as i64,
                self.download_id
            )
            .execute(&self.db)
            .await
            {
                consecutive_failures += 1;
                tracing::warn!(
                    ?err,
                    self.download_id,
                    "Supervisor: failed to write progress. Failed {consecutive_failures} of {MAX_CONSECUTIVE_FAILURES} times"
                );
            } else {
                consecutive_failures = 0;
            }

            if consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
                tracing::warn!(
                    "The supervisor for download #{} has failed consecutively {consecutive_failures} times. Aborting",
                    self.download_id
                );
                break;
            }

            if self.handle.managed.stats().finished {
                if let Err(err) = sqlx::query!(
                    "UPDATE downloads SET status = 'completed', completed_at = CURRENT_TIMESTAMP WHERE id = $1",
                    self.download_id
                )
                .execute(&self.db)
                .await
                {
                    tracing::error!(?err, self.download_id, "Supervisor: failed to mark completed");
                }
                tracing::info!(self.download_id, "Download completed");
                return;
            }

            tokio::select! {
                _ = self.cancel.cancelled() => {
                    tracing::debug!(self.download_id, "Supervisor cancelled");
                    return;
                }
                _ = tokio::time::sleep(Duration::from_secs(3)) => {}
            }
        }
    }
}
