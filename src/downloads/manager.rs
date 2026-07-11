//! Download lifecycle. The database is the source of truth; `Handle` owns
//! the in-flight supervisor map and a capacity semaphore, and exposes async
//! operations (`start`, `pause`, `cancel`, `remove`) that block until the
//! engine and DB are in the requested state.

use std::sync::Arc;

use super::TorrentEngine;
use crate::app::{Error, Pool, Storage};
use crate::config::Config;
use crate::downloads::types::DownloadStatus;
use crate::utils::supervisor_pool::{Acquire, SupervisorPool};

/// Result of a `start` attempt. `Started` is the only outcome that spawns a
/// new supervisor; the rest are idempotent no-ops the caller may want to
/// observe (e.g. surface "NoCapacity" to a UI as "queued").
#[derive(Debug, PartialEq, Eq)]
pub enum StartOutcome {
    Started,
    AlreadyRunning,
    AlreadyComplete { output_path: Option<String> },
    NoCapacity,
    Cancelled,
}

#[repr(u8)]
#[derive(Copy, Clone)]
pub enum DownloadPriority {
    Stream = 255,
    Background = 0,
}

/// Cheap, cloneable handle to the download subsystem.
#[derive(Clone)]
pub struct Handle(Arc<Inner>);

struct Inner {
    db: Pool,
    events: crate::Events,
    config: Arc<Config>,
    storage: Storage,
    supervisor_pool: SupervisorPool,
}

impl Handle {
    pub fn new(db: Pool, events: crate::Events, config: Arc<Config>, storage: Storage) -> Self {
        let permits = config.max_concurrent_downloads;
        let (supervisor_pool, refetch_rx) = SupervisorPool::new("download manager", permits);
        let inner = Arc::new(Inner {
            db,
            events,
            config,
            storage,
            supervisor_pool,
        });

        let weak = Arc::downgrade(&inner);
        inner.supervisor_pool.attach_refresh(refetch_rx, move || {
            let weak = weak.clone();
            async move {
                let Some(inner) = weak.upgrade() else {
                    return crate::utils::supervisor_pool::RefetchResult::Break;
                };

                Self(inner).refresh().await;

                crate::utils::supervisor_pool::RefetchResult::Continue
            }
        });

        Self(inner)
    }

    /// Cancel all in-flight supervisors and wait for them to drain.
    /// After this returns, no new downloads will be started.
    pub async fn shutdown(&self) {
        self.0.supervisor_pool.shutdown().await
    }

    /// Boot-time recovery: demote any rows left as `downloading` from a prior
    /// run back to `queued`, then schedule everything queued.
    pub async fn boot(&self) -> crate::app::Result<()> {
        let reset =
            sqlx::query!("UPDATE downloads SET status = 'queued' WHERE status = 'downloading'")
                .execute(&self.0.db)
                .await
                .map_err(Error::DatabaseError)?;

        if reset.rows_affected() > 0 {
            tracing::info!(
                count = reset.rows_affected(),
                "Reset interrupted downloads to queued"
            );
        }
        self.refresh().await;

        Ok(())
    }

    /// Upsert a download row. Reset it from any terminal state, and start it.
    /// Blocks until the supervisor is spawned (or returns a non-`Started` outcome).
    /// Returns the download id.
    pub async fn ensure_download(
        &self,
        info_hash: &str,
        file_idx: i32,
        priority: DownloadPriority,
    ) -> crate::app::Result<(i32, StartOutcome)> {
        let mut tx = self.0.db.begin().await.map_err(Error::DatabaseError)?;

        let id = super::types::Download::upsert(&mut tx, info_hash, file_idx).await?;

        super::types::Download::reset_for_restart(&mut tx, id).await?;

        tx.commit().await.map_err(Error::DatabaseError)?;

        Ok((id, self.start(id, priority).await?))
    }

    /// Start (or resume) a download. Blocks until the supervisor has been
    /// spawned and the engine has the torrent loaded and the requested file
    /// selected, or returns a non-`Started` outcome that explains why no
    /// supervisor was started.
    pub async fn start(
        &self,
        id: i32,
        priority: DownloadPriority,
    ) -> crate::app::Result<StartOutcome> {
        if self.0.supervisor_pool.is_running(id) {
            return Ok(StartOutcome::AlreadyRunning);
        }

        let row = sqlx::query!(
            r#"
                SELECT
                    info_hash,
                    file_idx,
                    status as "status: DownloadStatus",
                    output_path
                FROM downloads
                WHERE id = $1
            "#,
            id
        )
        .fetch_optional(&self.0.db)
        .await?
        .ok_or_else(|| Error::NotFound(format!("Download {id} not found")))?;

        if row.status == DownloadStatus::Completed {
            return Ok(StartOutcome::AlreadyComplete {
                output_path: row.output_path,
            });
        }

        // Claim the supervisor slot. Any concurrent start (for same download) will early return.
        let slot = self
            .0
            .supervisor_pool
            .acquire_evicting(id, priority as u8, |victim| async move {
                self.reenqueue(victim).await
            })
            .await?;

        let slot = match slot {
            Acquire::Acquired(slot) => slot,
            Acquire::AlreadyRunning => return Ok(StartOutcome::AlreadyRunning),
            Acquire::NoCapacity => return Ok(StartOutcome::NoCapacity),
        };

        let cancel = slot.cancel_token();
        let engine = TorrentEngine::get();
        let engine_key: super::engine::EngineKey = (row.info_hash, row.file_idx as usize).into();

        let engine_key_clone = engine_key.clone();
        let cancel_clone = cancel.clone();
        let start = async {
            let torrent = match engine
                .ensure_torrent(&engine_key.info_hash, &self.0.config)
                .await
            {
                Ok(h) => h,
                Err(err) => {
                    fail(&self.0.db, id, &err).await;
                    return Err(err);
                }
            };
            if let Err(err) = engine.select_file(&engine_key).await {
                fail(&self.0.db, id, &err).await;
                return Err(err);
            }

            let db = self.0.db.clone();
            let events = self.0.events.clone();
            let storage = self.0.storage.clone();
            slot.spawn(async move {
                super::supervisor::Supervisor::new(
                    db, events, storage, id, engine_key, torrent, cancel,
                )
                .await
                .run()
                .await;
            });

            Ok(StartOutcome::Started)
        };

        tokio::select! {
            biased;
            _ = cancel_clone.cancelled() => {
                if let Err(err) = engine.stop(&engine_key_clone).await {
                    tracing::warn!(?err, "Error while cleaning up in-flight start after cancellation");
                }

                Ok(StartOutcome::Cancelled)
            }
            res = start => res
        }
    }

    /// Pause downloading, keep files.
    pub async fn pause(&self, id: i32) -> crate::app::Result<()> {
        if self.0.supervisor_pool.cancel(id) {
            tracing::info!(id, "Pausing torrent");
        }
        if let Some(key) = fetch_download_key(&self.0.db, id).await
            && let Err(err) = TorrentEngine::get().pause(&key).await
        {
            tracing::debug!(?err, id, "Could not pause");
        }

        let res = sqlx::query!("UPDATE downloads SET status = 'paused' WHERE id = $1 AND status NOT IN ('completed', 'failed')", id)
            .execute(&self.0.db)
            .await?;

        if res.rows_affected() > 0 {
            self.emit_status_update(id, super::types::DownloadStatus::Paused);
        }

        Ok(())
    }

    /// Set the status (back) to [`DownloadStatus::Queued`]. This is useful when a background
    /// download needs to be temporarily postponed to make room for an active stream.
    pub async fn reenqueue(&self, id: i32) -> crate::app::Result<()> {
        if self.0.supervisor_pool.cancel(id) {
            tracing::info!(id, "Re-enqueueing torrent");
        }
        if let Some(key) = fetch_download_key(&self.0.db, id).await
            && let Err(err) = TorrentEngine::get().pause(&key).await
        {
            tracing::debug!(?err, id, "Could not re-enqueue");
        }

        let res = sqlx::query!("UPDATE downloads SET status = 'queued' WHERE id = $1 AND status NOT IN ('completed', 'failed')", id)
            .execute(&self.0.db)
            .await?;

        if res.rows_affected() > 0 {
            self.emit_status_update(id, super::types::DownloadStatus::Queued);
        }

        Ok(())
    }

    /// Stop downloading, keep files, releasing the resources
    pub async fn cancel(&self, id: i32) -> crate::app::Result<()> {
        if self.0.supervisor_pool.cancel(id) {
            tracing::info!(id, "Cancelling torrent");
        }
        if let Some(key) = fetch_download_key(&self.0.db, id).await
            && let Err(err) = TorrentEngine::get().stop(&key).await
        {
            tracing::warn!(?err, id, "Could not cancel");
        }

        let res = sqlx::query!(
            "UPDATE downloads SET status = 'cancelled' WHERE id = $1 AND status NOT IN ('completed', 'failed')",
            id
        )
        .execute(&self.0.db)
        .await?;

        if res.rows_affected() > 0 {
            self.emit_status_update(id, super::types::DownloadStatus::Cancelled);
        }

        Ok(())
    }

    /// Stop downloading, wipe files + row. Always destructive.
    pub async fn remove(&self, id: i32) -> crate::app::Result<()> {
        if self.0.supervisor_pool.cancel(id) {
            tracing::info!(id, "Cancelling torrent (remove)");
        }
        if let Some(key) = fetch_download_key(&self.0.db, id).await
            && let Err(err) = TorrentEngine::get().stop_and_delete(&key).await
        {
            tracing::warn!(?err, id, "Could not remove the torrent");
        }

        let res = sqlx::query!("DELETE FROM downloads WHERE id = $1", id)
            .execute(&self.0.db)
            .await?;

        if res.rows_affected() > 0 {
            self.0.events.downloads.emit_removed(&id);
        }

        Ok(())
    }

    /// Scan queued rows and try to start as many as fit under the
    /// concurrency cap. Spawns each start so slow engine I/O doesn't
    /// serialize across queued items.
    pub async fn refresh(&self) {
        let queued: Vec<i32> = match sqlx::query_scalar!(
            "SELECT id FROM downloads WHERE status = 'queued' ORDER BY created_at ASC"
        )
        .fetch_all(&self.0.db)
        .await
        {
            Ok(r) => r,
            Err(err) => {
                tracing::error!(?err, "Failed to query queued downloads");
                return;
            }
        };
        let take = self.0.supervisor_pool.available_capacity();
        for id in queued.into_iter().take(take) {
            let h = self.clone();
            self.0.supervisor_pool.spawn_helper(async move {
                if let Err(err) = h.start(id, DownloadPriority::Background).await {
                    tracing::warn!(?err, id, "Refresh: start failed");
                }
            });
        }
    }

    fn emit_status_update(&self, id: i32, new_status: super::types::DownloadStatus) {
        self.0
            .events
            .downloads
            .emit_status_update(&crate::api::downloads::DownloadStatusUpdate {
                download_id: id,
                new_status,
            });
    }
}

async fn fail(db: &Pool, id: i32, err: &Error) {
    tracing::error!(id, error = %err, "Download failed");
    if let Err(err) = sqlx::query!(
        "UPDATE downloads SET status = 'failed', error = $1 WHERE id = $2 AND status NOT IN ('cancelled', 'paused')",
        err.to_string(),
        id
    )
    .execute(db)
    .await
    {
        tracing::error!(?err, id, "Failed to record failure status");
    }
}

async fn fetch_download_key(db: &Pool, id: i32) -> Option<super::engine::EngineKey> {
    sqlx::query!(
        "SELECT info_hash, file_idx FROM downloads WHERE id = $1",
        id
    )
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .map(|record| (record.info_hash, record.file_idx as usize).into())
}
