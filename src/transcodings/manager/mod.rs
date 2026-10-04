//! Transcoding lifecycle. The DB is the source of truth for background pretranscodings,
//! `Handle` owns in-flight supervisors and a capacity semaphore, and each operation blocks
//! until the requested state is observable in both the process table and the DB.
//!
//! On top of the pretranscoding lifecycle, `Handle` also manages live HLS
//! sessions in an in-memory session map. A live session that re-encodes
//! video takes a slot in the same [`SupervisorPool`] at
//! [`TranscodingPriority::Live`], pre-empting background pretranscodings via
//! the pool's eviction path. Live sessions are never refused: when every
//! slot is held by another live session, one runs without a slot. Sessions
//! that only repackage are cheap and take no slot. Live sessions are intentionally *not* DB-backed: a session is a
//! pipeline serving a viewer and cannot survive a restart.

use std::collections::HashMap;
use std::sync::atomic::AtomicI32;
use std::sync::{Arc, Mutex};

use crate::app::{CinemaError, Pool, Storage};
use crate::config::Config;
use crate::transcodings::PretranscodingOutputPath;
use crate::transcodings::types::PretranscodingStatus;
use crate::utils::supervisor_pool::SupervisorPool;

mod background;
mod live;

pub use live::{ClientCapabilities, Playback};

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
    /// Pretranscodes suspended by a pause or a live eviction, by row id.
    parked: crate::transcodings::supervisor::ParkedJobs,
    /// In-memory live-session map, keyed by session_id.
    sessions: Mutex<HashMap<String, live::LiveSession>>,
    /// Probe results by file, so repeated playback calls don't re-demux. A
    /// probe in flight is shared: callers for the same file wait on it.
    probes: Mutex<HashMap<String, Arc<tokio::sync::OnceCell<Arc<media::MediaInfo>>>>>,
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
            parked: Default::default(),
            sessions: Mutex::new(HashMap::new()),
            probes: Mutex::new(HashMap::new()),
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
    /// tears down every live HLS session.
    pub async fn shutdown(&self) {
        self.stop_all_live().await;
        self.0.supervisor_pool.shutdown().await;
        self.0.parked.lock().unwrap().clear();
    }

    /// Boot-time recovery. Jobs run in-process, so anything left mid-flight
    /// (or suspended) by a previous run is gone: running rows go back to the
    /// queue and start over, paused rows start over when resumed. Also
    /// picks up any `queued` rows.
    pub async fn boot(&self) -> crate::app::Result<()> {
        // Warm the encoder choice up front: the first lookup runs test
        // encodes, which shouldn't land on the first stream's startup.
        let encoder = self.0.config.encoder();
        match tokio::task::spawn_blocking(move || media::h264_encoder_name(&encoder)).await {
            Ok(Some(name)) => tracing::info!(encoder = name, "Video encoder selected"),
            _ => tracing::warn!("No working H.264 encoder; video transcoding will fail"),
        }

        let interrupted = sqlx::query!(
            r#"
                UPDATE pretranscodings
                SET status = CASE WHEN status = 'transcoding' THEN 'queued'::pretranscoding_status ELSE status END,
                    transcoded_ms = 0
                WHERE status IN ('transcoding', 'paused')
                RETURNING download_id, only_audio, audio_index
            "#,
        )
        .fetch_all(&self.0.db)
        .await
        .map_err(CinemaError::DatabaseError)?;

        for row in &interrupted {
            PretranscodingOutputPath::new(
                &self.0.storage,
                row.download_id,
                row.only_audio,
                row.audio_index,
            )
            .remove()
            .await;
        }
        if !interrupted.is_empty() {
            tracing::info!(
                count = interrupted.len(),
                "Restarting pretranscodings interrupted by the restart"
            );
        }

        // Live sessions don't survive a restart either.
        let _ = tokio::fs::remove_dir_all(self.0.storage.hls_dir()).await;

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

    /// Probes a file, caching the result per `key`. Concurrent callers share
    /// one probe: a torrent that is still downloading can take minutes to
    /// deliver its header, and the player asks repeatedly meanwhile. Failed
    /// probes and ones without a duration aren't kept, so the next call
    /// tries again.
    pub(crate) async fn media_info(
        &self,
        key: &str,
        input: &media::Input,
    ) -> crate::app::Result<Arc<media::MediaInfo>> {
        let cell = {
            let mut probes = self.0.probes.lock().unwrap();
            if probes.len() > 512 {
                probes.retain(|_, cell| !cell.initialized());
            }
            probes.entry(key.to_string()).or_default().clone()
        };
        let info = cell
            .get_or_try_init(|| async { media::probe(input).await.map(Arc::new) })
            .await
            .cloned();
        let keep = info.as_ref().is_ok_and(|i| i.duration.is_some());
        if !keep {
            let mut probes = self.0.probes.lock().unwrap();
            if probes.get(key).is_some_and(|c| Arc::ptr_eq(c, &cell)) {
                probes.remove(key);
            }
        }
        Ok(info?)
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
