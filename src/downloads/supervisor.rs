use std::time::Duration;

use crate::app::{Pool, Storage};

/// Streaming progress for an active download. Emitted periodically by the
/// download supervisor
#[draad::ty]
pub struct DownloadProgress {
    download_id: i32,
    downloaded_bytes: u64,
    total_bytes: u64,
    download_speed_mbps: Option<f64>,
    status: super::types::DownloadStatus,
}

pub struct Supervisor {
    download_id: i32,
    info_hash: String,
    file_idx: i32,
    db: Pool,
    storage: Storage,
    events: crate::Events,
    handle: super::TorrentHandle,
    cancel: tokio_util::sync::CancellationToken,
}

impl Supervisor {
    pub async fn new(
        db: Pool,
        events: crate::Events,
        storage: Storage,
        download_id: i32,
        engine_key: super::engine::EngineKey,
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
            info_hash: engine_key.info_hash,
            file_idx: engine_key.file_idx as i32,
            db,
            storage,
            events,
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
            let stats = self.handle.managed.stats();
            if let Err(err) = sqlx::query!(
                "UPDATE downloads SET downloaded_bytes = $1, total_bytes = $2 WHERE id = $3 AND status = 'downloading'",
                stats.progress_bytes as i64,
                stats.total_bytes as i64,
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

            self.events.downloads.emit_progress(&DownloadProgress {
                download_id: self.download_id,
                downloaded_bytes: stats.progress_bytes,
                total_bytes: stats.total_bytes,
                download_speed_mbps: stats.live.map(|live| live.download_speed.mbps),
                status: if stats.finished {
                    super::types::DownloadStatus::Completed
                } else {
                    super::types::DownloadStatus::Downloading
                },
            });

            if stats.finished {
                // Resolve the on-disk path while the torrent is still loaded,
                // and persist it storage-relative so consumers can bypass the
                // engine for completed downloads.
                let engine = super::TorrentEngine::get();
                let output_path = match engine.file_path(&self.info_hash, self.file_idx as usize) {
                    Ok(abs) => Some(
                        abs.strip_prefix(self.storage.path())
                            .map(|p| p.to_string_lossy().into_owned())
                            .unwrap_or_else(|_| abs.to_string_lossy().into_owned()),
                    ),
                    Err(err) => {
                        tracing::warn!(
                            ?err,
                            self.download_id,
                            "Supervisor: failed to resolve output_path"
                        );
                        None
                    }
                };

                if let Err(err) = sqlx::query!(
                    "UPDATE downloads SET status = 'completed', completed_at = CURRENT_TIMESTAMP, output_path = $2 WHERE id = $1",
                    self.download_id,
                    output_path,
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
