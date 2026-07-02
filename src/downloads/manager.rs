//! Download lifecycle. The database is the source of truth; `Handle` owns
//! the in-flight supervisor map and a capacity semaphore, and exposes async
//! operations (`start`, `pause`, `cancel`, `remove`) that block until the
//! engine and DB are in the requested state.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Semaphore, mpsc};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use super::TorrentEngine;
use crate::app::{Error, Pool};
use crate::config::Config;
use crate::downloads::supervisor_guard::SupervisorGuard;
use crate::downloads::types::DownloadStatus;

/// Result of a `start` attempt. `Started` is the only outcome that spawns a
/// new supervisor; the rest are idempotent no-ops the caller may want to
/// observe (e.g. surface "NoCapacity" to a UI as "queued").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartOutcome {
    Started,
    AlreadyRunning,
    AlreadyComplete,
    NoCapacity,
    Cancelled,
}

/// Cheap, cloneable handle to the download subsystem.
#[derive(Clone)]
pub struct Handle(Arc<Inner>);

struct Inner {
    db: Pool,
    config: Arc<Config>,
    semaphore: Arc<Semaphore>,
    supervisors: std::sync::Mutex<HashMap<i32, CancellationToken>>,
    refresh_tx: mpsc::Sender<()>,
    shutdown: CancellationToken,
    tracker: TaskTracker,
}

impl Handle {
    pub fn new(db: Pool, config: Arc<Config>) -> Self {
        let permits = config.max_concurrent_downloads;
        let (refresh_tx, mut refresh_rx) = mpsc::channel::<()>(64);
        let inner = Arc::new(Inner {
            db,
            config,
            semaphore: Arc::new(Semaphore::new(permits)),
            supervisors: std::sync::Mutex::new(HashMap::new()),
            refresh_tx,
            shutdown: CancellationToken::new(),
            tracker: TaskTracker::new(),
        });

        let weak = Arc::downgrade(&inner);
        let shutdown = inner.shutdown.clone();
        inner.tracker.spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    _ = shutdown.cancelled() => break,
                    msg = refresh_rx.recv() => {
                        if msg.is_none() {
                            break;
                        }
                        let Some(inner) = weak.upgrade() else {
                            break;
                        };
                        Self(inner).refresh().await;
                    }
                }
            }
        });

        Self(inner)
    }

    /// Cancel all in-flight supervisors and wait for them to drain.
    /// After this returns, no new downloads will be started.
    pub async fn shutdown(&self) {
        self.0.shutdown.cancel();
        self.0.tracker.close();
        if let Err(err) = tokio::time::timeout(Duration::from_secs(5), self.0.tracker.wait()).await
        {
            tracing::error!(?err, "Download manager shutdown timed out");
        }
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

    /// Start (or resume) a download. Blocks until the supervisor has been
    /// spawned and the engine has the torrent loaded and the requested file
    /// selected, or returns a non-`Started` outcome that explains why no
    /// supervisor was started.
    pub async fn start(&self, id: i32) -> crate::app::Result<StartOutcome> {
        if self.0.supervisors.lock().unwrap().contains_key(&id) {
            return Ok(StartOutcome::AlreadyRunning);
        }

        let row = load_min(&self.0.db, id)
            .await
            .ok_or_else(|| Error::NotFound(format!("Download {id} not found")))?;

        if row.status == DownloadStatus::Completed {
            return Ok(StartOutcome::AlreadyComplete);
        }

        // Claim the supervisor slot. Any concurrent start (for same download) will early return.
        // Child of the manager-wide shutdown token so a single `shutdown.cancel()` cascades
        // to every live supervisor.
        let cancel = self.0.shutdown.child_token();
        let guard = {
            let mut sup = self.0.supervisors.lock().unwrap();
            if sup.contains_key(&id) {
                return Ok(StartOutcome::AlreadyRunning);
            }
            sup.insert(id, cancel.clone());

            SupervisorGuard::new(|| {
                if let Ok(mut supervisors) = self.0.supervisors.lock()
                    && let Some(cancel) = supervisors.remove(&id)
                {
                    cancel.cancel();
                } else {
                    tracing::warn!("The guard could not clean up the supervisor for download #{id}")
                }
            })
        };

        // Reserve capacity before doing any slow engine work. If we can't,
        // leave the row as-is so a future refresh retries it.
        let permit = match self.0.semaphore.clone().try_acquire_owned() {
            Ok(p) => p,
            Err(_) => return Ok(StartOutcome::NoCapacity),
        };

        let engine = TorrentEngine::get();

        let cancel_clone = cancel.clone();
        let start = async {
            let torrent = match engine.ensure_torrent(&row.info_hash, &self.0.config).await {
                Ok(h) => h,
                Err(err) => {
                    fail(&self.0.db, id, &err).await;
                    return Err(err);
                }
            };
            if let Err(err) = engine.resume(&row.info_hash).await {
                tracing::warn!(?err, info_hash = %row.info_hash, "engine.resume failed");
            }
            if let Err(err) = engine
                .select_file(&row.info_hash, row.file_idx as usize)
                .await
            {
                fail(&self.0.db, id, &err).await;
                return Err(err);
            }

            guard.commit();

            let inner = self.0.clone();
            let db = self.0.db.clone();
            self.0.tracker.spawn(async move {
                super::supervisor::Supervisor::new(db, id, torrent, cancel)
                    .await
                    .run()
                    .await;
                drop(permit);
                inner.supervisors.lock().unwrap().remove(&id);
                // A slot just freed up, kick the coordinator to refresh.
                let _ = inner.refresh_tx.send(()).await;
            });

            Ok(StartOutcome::Started)
        };

        tokio::select! {
            biased;
            _ = cancel_clone.cancelled() => {
                if let Err(err) = engine.stop(&row.info_hash).await {
                    tracing::warn!(?err, "Error while cleaning up in-flight start after cancellation");
                }

                Ok(StartOutcome::Cancelled)
            }
            res = start => res
        }
    }

    /// Pause downloading, keep files.
    pub async fn pause(&self, id: i32) -> crate::app::Result<()> {
        if let Some(cancel) = self.0.supervisors.lock().unwrap().remove(&id) {
            tracing::info!(id, "Pausing torrent");
            cancel.cancel();
        }
        if let Some(hash) = fetch_info_hash(&self.0.db, id).await
            && let Err(err) = TorrentEngine::get().pause(&hash).await
        {
            tracing::debug!(?err, id, "Counld not pause");
        }
        sqlx::query!("UPDATE downloads SET status = 'paused' WHERE id = $1 AND status NOT IN ('completed', 'failed')", id)
            .execute(&self.0.db)
            .await
            .map_err(Error::DatabaseError)?;
        Ok(())
    }

    /// Stop downloading, keep files, releasing the resources
    pub async fn cancel(&self, id: i32) -> crate::app::Result<()> {
        if let Some(cancel) = self.0.supervisors.lock().unwrap().remove(&id) {
            tracing::info!(id, "Cancelling torrent");
            cancel.cancel();
        }
        if let Some(hash) = fetch_info_hash(&self.0.db, id).await
            && let Err(err) = TorrentEngine::get().stop(&hash).await
        {
            tracing::debug!(?err, id, "Could not cancel");
        }
        sqlx::query!(
            "UPDATE downloads SET status = 'cancelled' WHERE id = $1 AND status NOT IN ('completed', 'failed')",
            id
        )
        .execute(&self.0.db)
        .await
        .map_err(Error::DatabaseError)?;
        Ok(())
    }

    /// Stop downloading, wipe files + row. Always destructive.
    pub async fn remove(&self, id: i32) -> crate::app::Result<()> {
        if let Some(cancel) = self.0.supervisors.lock().unwrap().remove(&id) {
            tracing::info!(id, "Cancelling torrent (remove)");
            cancel.cancel();
        }
        if let Some(hash) = fetch_info_hash(&self.0.db, id).await {
            TorrentEngine::get().stop_and_delete(&hash).await;
        }
        sqlx::query!("DELETE FROM downloads WHERE id = $1", id)
            .execute(&self.0.db)
            .await
            .map_err(Error::DatabaseError)?;
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
        let take = self.0.semaphore.available_permits();
        for id in queued.into_iter().take(take) {
            let h = self.clone();
            self.0.tracker.spawn(async move {
                if let Err(err) = h.start(id).await {
                    tracing::warn!(?err, id, "Refresh: start failed");
                }
            });
        }
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

async fn fetch_info_hash(db: &Pool, id: i32) -> Option<String> {
    sqlx::query_scalar!("SELECT info_hash FROM downloads WHERE id = $1", id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
}

struct MinRow {
    info_hash: String,
    file_idx: i32,
    status: DownloadStatus,
}

async fn load_min(db: &Pool, id: i32) -> Option<MinRow> {
    let res = sqlx::query!(
        r#"
        SELECT
            d.info_hash,
            d.file_idx,
            d.status as "status: DownloadStatus"
        FROM downloads d
        WHERE d.id = $1
        "#,
        id
    )
    .fetch_optional(db)
    .await
    .ok()
    .flatten()?;
    Some(MinRow {
        info_hash: res.info_hash,
        file_idx: res.file_idx,
        status: res.status,
    })
}
