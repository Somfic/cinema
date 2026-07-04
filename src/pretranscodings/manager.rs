//! Pretranscoding lifecycle. Mirrors [`crate::downloads::manager`]: the DB is
//! the source of truth, `Handle` owns in-flight supervisors and a capacity
//! semaphore, and each operation blocks until the requested state is
//! observable in both the process table and the DB.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Semaphore, mpsc};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::app::{Error, Pool, Storage};
use crate::config::Config;
use crate::pretranscodings::PretranscodingOutputPath;
use crate::pretranscodings::supervisor::Supervisor;
use crate::pretranscodings::supervisor_guard::SupervisorGuard;
use crate::pretranscodings::types::PretranscodingStatus;

/// Cheap, cloneable handle to the pretranscoding subsystem.
#[derive(Clone)]
pub struct Handle(Arc<Inner>);

struct Inner {
    db: Pool,
    events: crate::Events,
    config: Arc<Config>,
    storage: Storage,
    semaphore: Arc<Semaphore>,
    supervisors: std::sync::Mutex<HashMap<i32, CancellationToken>>,
    refresh_tx: mpsc::Sender<()>,
    shutdown: CancellationToken,
    tracker: TaskTracker,
}

impl Handle {
    pub fn new(db: Pool, events: crate::Events, config: Arc<Config>, storage: Storage) -> Self {
        let permits = config.max_concurrent_pretranscodings.max(1);
        let (refresh_tx, mut refresh_rx) = mpsc::channel::<()>(64);
        let inner = Arc::new(Inner {
            db,
            events,
            config,
            storage,
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
                        let Some(inner) = weak.upgrade() else { break };
                        Self(inner).refresh().await;
                    }
                }
            }
        });

        Self(inner)
    }

    /// Cancel all in-flight supervisors and wait for them to drain.
    pub async fn shutdown(&self) {
        self.0.shutdown.cancel();
        self.0.tracker.close();
        if let Err(err) = tokio::time::timeout(Duration::from_secs(5), self.0.tracker.wait()).await
        {
            tracing::error!(?err, "Pretranscoding manager shutdown timed out");
        }
    }

    /// Boot-time recovery. A partial MP4 without its moov atom is unusable, so
    /// any row left mid-flight from a previous run is marked failed and its
    /// `.part` file is scrubbed. Also picks up any `queued` rows.
    pub async fn boot(&self) -> crate::app::Result<()> {
        // Collect the rows we're about to fail so we can also delete their
        // partial output files.
        let interrupted = sqlx::query!(
            r#"
                SELECT pt.id, pt.download_id, pt.only_audio, pt.audio_index
                FROM pretranscodings pt
                WHERE pt.status IN ('queued', 'transcoding')
            "#,
        )
        .fetch_all(&self.0.db)
        .await
        .map_err(Error::DatabaseError)?;

        for row in &interrupted {
            let path = PretranscodingOutputPath::new(
                &self.0.storage,
                row.download_id,
                row.only_audio,
                row.audio_index,
            );
            if let Err(err) = tokio::fs::remove_file(path.with_extension("mp4.part")).await {
                tracing::warn!(?err, ?path, "Could not remove partial pretranscoding");
            }
        }

        let reset = sqlx::query!(
            "UPDATE pretranscodings SET status = 'failed', error = 'Interrupted at restart' WHERE status IN ('queued', 'transcoding')",
        )
        .execute(&self.0.db)
        .await
        .map_err(Error::DatabaseError)?;

        if reset.rows_affected() > 0 {
            tracing::info!(
                count = reset.rows_affected(),
                "Marked interrupted pretranscodings as failed"
            );
        }
        Ok(())
    }

    /// Queue a pretranscoding for the given download + audio track + mode.
    /// Idempotent - a duplicate returns the existing row id.
    pub async fn enqueue(
        &self,
        download_id: i32,
        only_audio: bool,
        audio_index: i32,
    ) -> crate::app::Result<i32> {
        // TODO: TOCTOU with remove/remove_all_for_download vulnerable. Should add database locking

        // Verify the download exists (fail early rather than dangling FK).
        let exists = sqlx::query_scalar!(
            "SELECT EXISTS(SELECT 1 FROM downloads WHERE id = $1) as \"exists!\"",
            download_id
        )
        .fetch_one(&self.0.db)
        .await
        .map_err(Error::DatabaseError)?;

        if !exists {
            return Err(Error::NotFound(format!("Download {download_id} not found")));
        }

        // If a queued/transcoding/completed row exists, return it.
        // Otherwise create a new queued row (upserting over any prior terminal-state row for the same key).
        let existing = sqlx::query_scalar!(
            r#"
                SELECT id
                FROM pretranscodings
                WHERE download_id = $1
                    AND only_audio = $2
                    AND audio_index = $3
                    AND status in ('queued', 'transcoding', 'completed')
            "#,
            download_id,
            only_audio,
            audio_index,
        )
        .fetch_optional(&self.0.db)
        .await
        .map_err(Error::DatabaseError)?;

        if let Some(id) = existing {
            return Ok(id);
        }

        let id = sqlx::query_scalar!(
            r#"
                INSERT INTO pretranscodings (download_id, only_audio, audio_index)
                VALUES ($1, $2, $3)
                    ON CONFLICT (download_id, only_audio, audio_index) DO UPDATE SET
                        status = DEFAULT,
                        error = DEFAULT,
                        transcoded_ms = DEFAULT,
                        total_ms = DEFAULT,
                        completed_at = DEFAULT,
                        created_at = DEFAULT
                RETURNING id
            "#,
            download_id,
            only_audio,
            audio_index,
        )
        .fetch_one(&self.0.db)
        .await
        .map_err(Error::DatabaseError)?;

        self.emit_status_update(id, download_id, PretranscodingStatus::Queued);
        let _ = self.0.refresh_tx.send(()).await;
        Ok(id)
    }

    /// Cancel a running/queued pretranscoding. Deletes partial output; leaves
    /// the row in `cancelled` state so the user can see what happened.
    pub async fn cancel(&self, id: i32) -> crate::app::Result<()> {
        if let Some(cancel) = self.0.supervisors.lock().unwrap().remove(&id) {
            cancel.cancel();
        }

        let row = sqlx::query!(
            "SELECT download_id, only_audio, audio_index FROM pretranscodings WHERE id = $1",
            id
        )
        .fetch_optional(&self.0.db)
        .await
        .map_err(Error::DatabaseError)?;

        if let Some(row) = &row {
            let path = PretranscodingOutputPath::new(
                &self.0.storage,
                row.download_id,
                row.only_audio,
                row.audio_index,
            );
            let _ = tokio::fs::remove_file(path.with_extension("mp4.part")).await;
        }

        sqlx::query!(
            "UPDATE pretranscodings SET status = 'cancelled' WHERE id = $1 AND status NOT IN ('completed', 'failed')",
            id,
        )
        .execute(&self.0.db)
        .await
        .map_err(Error::DatabaseError)?;

        if let Some(row) = row {
            self.emit_status_update(id, row.download_id, PretranscodingStatus::Cancelled);
        }

        Ok(())
    }

    /// Cancel if running, delete the row, and remove any cached files.
    pub async fn remove(&self, id: i32) -> crate::app::Result<()> {
        if let Some(cancel) = self.0.supervisors.lock().unwrap().remove(&id) {
            cancel.cancel();
        }

        let row = sqlx::query!(
            "SELECT download_id, only_audio, audio_index FROM pretranscodings WHERE id = $1",
            id
        )
        .fetch_optional(&self.0.db)
        .await
        .map_err(Error::DatabaseError)?;

        if let Some(row) = &row {
            let path = PretranscodingOutputPath::new(
                &self.0.storage,
                row.download_id,
                row.only_audio,
                row.audio_index,
            );
            if let Err(err) = tokio::fs::remove_file(&path).await {
                tracing::warn!(?err, "Could not remove the pretranscoding file");
            }
            if let Err(err) = tokio::fs::remove_file(path.with_extension("mp4.part")).await {
                tracing::warn!(?err, "Could not remove the pretranscoding part file");
            }
        }

        sqlx::query!("DELETE FROM pretranscodings WHERE id = $1", id)
            .execute(&self.0.db)
            .await
            .map_err(Error::DatabaseError)?;

        if let Some(row) = row {
            self.0.events.pretranscodings.emit_removed(
                &crate::api::pretranscodings::PretranscodingRemoved {
                    pretranscoding_id: id,
                    download_id: row.download_id,
                },
            );
        }

        Ok(())
    }

    /// Cancel every supervisor for a download and delete its cached files.
    /// Called from `downloads::Handle::remove` before the `ON DELETE CASCADE`
    /// drops the pretranscoding rows.
    pub async fn remove_all_for_download(&self, download_id: i32) -> crate::app::Result<()> {
        let rows = sqlx::query!(
            "SELECT id, only_audio, audio_index FROM pretranscodings WHERE download_id = $1",
            download_id,
        )
        .fetch_all(&self.0.db)
        .await
        .map_err(Error::DatabaseError)?;

        {
            let mut sup = self.0.supervisors.lock().unwrap();
            for row in &rows {
                if let Some(cancel) = sup.remove(&row.id) {
                    cancel.cancel();
                }
            }
        }

        for row in &rows {
            let path = PretranscodingOutputPath::new(
                &self.0.storage,
                download_id,
                row.only_audio,
                row.audio_index,
            );
            if let Err(err) = tokio::fs::remove_file(&path).await {
                tracing::warn!(?err, "Could not remove the pretranscoding file");
            }
            if let Err(err) = tokio::fs::remove_file(path.with_extension("mp4.part")).await {
                tracing::warn!(?err, "Could not remove the pretranscoding part file");
            }
        }

        Ok(())
    }

    /// Scan queued rows and start as many as fit under the concurrency cap.
    async fn refresh(&self) {
        let queued: Vec<i32> = match sqlx::query_scalar!(
            "SELECT id FROM pretranscodings WHERE status = 'queued' ORDER BY created_at ASC"
        )
        .fetch_all(&self.0.db)
        .await
        {
            Ok(r) => r,
            Err(err) => {
                tracing::error!(?err, "Failed to query queued pretranscodings");
                return;
            }
        };
        let take = self.0.semaphore.available_permits();
        for id in queued.into_iter().take(take) {
            let h = self.clone();
            self.0.tracker.spawn(async move {
                if let Err(err) = h.start(id).await {
                    tracing::warn!(?err, id, "Pretranscoding refresh: start failed");
                }
            });
        }
    }

    async fn start(&self, id: i32) -> crate::app::Result<()> {
        if self.0.supervisors.lock().unwrap().contains_key(&id) {
            return Ok(());
        }

        let row = sqlx::query!(
            r#"
                SELECT
                    pt.download_id,
                    pt.only_audio,
                    pt.audio_index,
                    pt.status as "status: PretranscodingStatus",
                    d.info_hash,
                    d.file_idx
                FROM pretranscodings pt
                JOIN downloads d ON d.id = pt.download_id
                WHERE pt.id = $1
            "#,
            id,
        )
        .fetch_optional(&self.0.db)
        .await
        .map_err(Error::DatabaseError)?
        .ok_or_else(|| Error::NotFound(format!("Pretranscoding {id} not found")))?;

        if row.status != PretranscodingStatus::Queued {
            return Ok(());
        }

        let cancel = self.0.shutdown.child_token();
        let guard = {
            let mut sup = self.0.supervisors.lock().unwrap();
            if sup.contains_key(&id) {
                return Ok(());
            }
            sup.insert(id, cancel.clone());

            SupervisorGuard::new(|| {
                if let Ok(mut supervisors) = self.0.supervisors.lock()
                    && let Some(cancel) = supervisors.remove(&id)
                {
                    cancel.cancel();
                } else {
                    tracing::warn!(
                        "The guard could not clean up the supervisor for pretranscoding #{id}"
                    );
                }
            })
        };

        let permit = match self.0.semaphore.clone().try_acquire_owned() {
            Ok(p) => p,
            Err(_) => return Ok(()),
        };

        // TODO: this shouldn't happen this way
        // Kick the torrent into life so the blocking reader in the supervisor
        // has something to draw from.
        let engine = crate::downloads::TorrentEngine::get();
        engine
            .ensure_torrent(&row.info_hash, &self.0.config)
            .await?;
        let _ = engine.resume(&row.info_hash).await;
        engine
            .select_file(&row.info_hash, row.file_idx as usize)
            .await?;

        guard.commit();

        let inner = self.0.clone();
        let supervisor = Supervisor::new(
            self.0.db.clone(),
            self.0.events.clone(),
            self.0.config.clone(),
            id,
            (row.info_hash, row.file_idx),
            PretranscodingOutputPath::new(
                &self.0.storage,
                row.download_id,
                row.only_audio,
                row.audio_index,
            ),
            cancel,
        );

        self.0.tracker.spawn(async move {
            supervisor.run().await;
            drop(permit);
            inner.supervisors.lock().unwrap().remove(&id);
            let _ = inner.refresh_tx.send(()).await;
        });

        Ok(())
    }

    fn emit_status_update(&self, id: i32, download_id: i32, new_status: PretranscodingStatus) {
        self.0.events.pretranscodings.emit_status_update(
            &crate::api::pretranscodings::PretranscodingStatusUpdate {
                pretranscoding_id: id,
                download_id,
                new_status,
            },
        );
    }
}
