//! Transcoding lifecycle. The DB is the source of truth for background pretranscodings,
//! `Handle` owns in-flight supervisors and a capacity semaphore, and each operation blocks
//! until the requested state is observable in both the process table and the DB.
//!
//! On top of the pretranscoding lifecycle, `Handle` also manages live HLS
//! sessions in an in-memory session map. Live sessions occupy a slot in the
//! same [`SupervisorPool`] at [`TranscodingPriority::Live`] and pre-empt
//! background pretranscodings via the pool's eviction path. Live sessions are
//! intentionally *not* DB-backed: a session is a running ffmpeg process
//! serving a viewer and cannot survive a restart (the process dies, the temp
//! segments are stale, the browser reconnects to a fresh one).

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicI32;

use crate::app::{Error, Pool, Storage};
use crate::config::Config;
use crate::transcodings::PretranscodingOutputPath;
use crate::transcodings::session::SessionMap;
use crate::transcodings::types::PretranscodingStatus;
use crate::utils::supervisor_pool::SupervisorPool;

mod background;
mod live;

/// Priority ranking for the transcoding [`SupervisorPool`]. A live viewer session pre-empts
/// any background pretranscoding, background pretranscodings never evict
/// anything.
///
/// [`SupervisorPool`]: crate::utils::supervisor_pool::SupervisorPool
#[repr(u8)]
#[derive(Copy, Clone, Debug)]
pub enum TranscodingPriority {
    /// Active viewer stream. Runs immediately, may evict pretranscodings.
    Live = 255,
    /// Background pretranscode. Runs when capacity is free; evictable.
    Pretranscoding = 0,
}

/// Handle returned from `Handle::start_playback`. The session is owned by the
/// manager; consumers use `session_id` to address it (touch / stop / stream
/// segments) and `playlist_url` to hand off to the video element.
#[derive(Debug, Clone)]
pub struct PlaybackSession {
    pub session_id: String,
    pub playlist_url: String,
}

/// Cheap, cloneable handle to the transcoding subsystem.
#[derive(Clone)]
pub struct Handle(Arc<Inner>);

struct Inner {
    db: Pool,
    events: crate::Events,
    downloads_manager: crate::downloads::Handle,
    config: Arc<Config>,
    storage: Storage,
    supervisor_pool: SupervisorPool,
    /// In-memory live-session map, keyed by session_id.
    sessions: SessionMap,
    /// Monotonic negative counter for `SupervisorPool` keys used by live
    /// sessions. Pretranscoding IDs are Postgres SERIAL (always > 0), so
    /// staying negative guarantees no collision.
    live_pool_id: AtomicI32,
}

impl Handle {
    pub fn new(
        db: Pool,
        events: crate::Events,
        downloads_manager: crate::downloads::Handle,
        config: Arc<Config>,
        storage: Storage,
    ) -> Self {
        let permits = config.max_concurrent_pretranscodings.max(1);
        let (supervisor_pool, refetch_rx) = SupervisorPool::new("transcodings manager", permits);
        let inner = Arc::new(Inner {
            db,
            events,
            downloads_manager,
            config,
            storage,
            supervisor_pool,
            sessions: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            live_pool_id: AtomicI32::new(-1),
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

    /// Cancel all in-flight supervisors and wait for them to drain. Also
    /// tears down every live HLS session so their ffmpeg children die and
    /// their temp segment directories are removed.
    pub async fn shutdown(&self) {
        self.stop_all_live().await;
        self.0.supervisor_pool.shutdown().await
    }

    /// Boot-time recovery. A partial MP4 without its moov atom is unusable, so
    /// any row left mid-flight from a previous run is marked failed and its
    /// segment files are scrubbed. Also picks up any `queued` rows. Rows in
    /// `paused` state are left alone: their segments were finalized cleanly
    /// on the pause SIGINT path and are safe to resume.
    pub async fn boot(&self) -> crate::app::Result<()> {
        // Collect the rows we're about to fail so we can also delete their
        // partial output files.
        let interrupted = sqlx::query!(
            r#"
                SELECT pt.id, pt.download_id, pt.only_audio, pt.audio_index
                FROM pretranscodings pt
                WHERE pt.status = 'transcoding'
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
            path.remove_all_segments().await;
        }

        let reset = sqlx::query!(
            "UPDATE pretranscodings SET status = 'failed', error = 'Interrupted at restart' WHERE status = 'transcoding'",
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

        self.refresh().await;

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
        let take = self.0.supervisor_pool.available_capacity();
        for id in queued.into_iter().take(take) {
            let h = self.clone();
            self.0.supervisor_pool.spawn_helper(async move {
                if let Err(err) = h.start(id).await {
                    tracing::warn!(?err, id, "Pretranscoding refresh: start failed");
                }
            });
        }
    }

    fn emit_status_update(&self, id: i32, download_id: i32, new_status: PretranscodingStatus) {
        self.0.events.transcodings.emit_status_update(
            &crate::api::transcodings::PretranscodingStatusUpdate {
                pretranscoding_id: id,
                download_id,
                new_status,
            },
        );
    }
}
