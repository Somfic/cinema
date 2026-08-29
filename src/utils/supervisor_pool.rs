//! Shared concurrency primitives for supervised job pools (downloads,
//! pretranscodings). Owns capacity, per-id cancellation, the task tracker,
//! and the refresh nudge loop; managers layer domain logic on top.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

/// Outcome of [`SupervisorPool::try_acquire`].
pub enum Acquire {
    Acquired(Slot),
    AlreadyRunning,
    NoCapacity,
}

pub enum RefetchResult {
    Continue,
    Break,
}

/// Reservation for one supervisor slot. Dropping without `spawn` releases
/// the map entry, the semaphore permit, and cancels the token.
pub struct Slot {
    id: i32,
    permit: Option<OwnedSemaphorePermit>, // None once `spawn` has taken it
    cancel: CancellationToken,
    pool: Arc<PoolInner>,
}

impl Slot {
    /// Child of the pool's shutdown token. Awaitable during setup so slow
    /// engine work can abort cleanly when `pool.cancel(id)` fires.
    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel.clone()
    }

    /// Consume the slot and run `fut` on the pool's tracker. On completion
    /// the map entry + permit are released and refresh is nudged.
    pub fn spawn<F>(mut self, fut: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let permit = self.permit.take().expect("slot already spawned");
        let id = self.id;
        let pool = self.pool.clone();
        self.pool.tracker.spawn(async move {
            // Guard in case fut panics and is dropped
            let _guard = super::guard::Guard::new(|| {
                pool.supervisors.lock().unwrap().remove(&id);
                let _ = pool.refresh_tx.try_send(());
            });
            fut.await;
            drop(permit);
        });
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        // permit == None ⇒ spawn consumed us; nothing to clean up.
        if self.permit.is_some()
            && let Ok(mut supervisors) = self.pool.supervisors.lock()
        {
            if let Some(entry) = supervisors.remove(&self.id) {
                entry.cancel.cancel();
                let _ = self.pool.refresh_tx.try_send(());
            } else if !self.cancel.is_cancelled() {
                tracing::warn!(
                    "The slot could not clean up the supervisor for entry {} of {}",
                    self.id,
                    self.pool.subsystem
                );
            }
        }
    }
}

struct SlotEntry {
    cancel: CancellationToken,
    priority: u8,
    started_at: Instant,
}

#[derive(Clone)]
pub struct SupervisorPool(Arc<PoolInner>);

struct PoolInner {
    subsystem: &'static str,
    semaphore: Arc<Semaphore>,
    supervisors: Mutex<HashMap<i32, SlotEntry>>,
    acquire_evict_mutex: tokio::sync::Mutex<()>,
    refresh_tx: mpsc::Sender<()>,
    shutdown: CancellationToken,
    tracker: TaskTracker,
}

impl SupervisorPool {
    pub fn new(subsystem: &'static str, capacity: usize) -> (Self, mpsc::Receiver<()>) {
        let (refresh_tx, refresh_rx) = mpsc::channel::<()>(64);
        (
            Self(Arc::new(PoolInner {
                subsystem,
                semaphore: Arc::new(Semaphore::new(capacity.max(1))),
                supervisors: Mutex::new(HashMap::new()),
                acquire_evict_mutex: tokio::sync::Mutex::new(()),
                refresh_tx,
                shutdown: CancellationToken::new(),
                tracker: TaskTracker::new(),
            })),
            refresh_rx,
        )
    }

    /// Register the refresh callback exactly once, after the manager
    /// exists. Closure typically captures `Weak<Manager>` to break the
    /// ref cycle (same pattern the current managers already use).
    pub fn attach_refresh<F, Fut>(&self, mut refetch_rx: mpsc::Receiver<()>, mut on_refresh: F)
    where
        F: FnMut() -> Fut + Send + 'static,
        Fut: Future<Output = RefetchResult> + Send,
    {
        let shutdown = self.0.shutdown.clone();
        self.0.tracker.spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    _ = shutdown.cancelled() => break,
                    msg = refetch_rx.recv() => {
                        if msg.is_none() {
                            break;
                        }
                        match on_refresh().await {
                            RefetchResult::Continue => {},
                            RefetchResult::Break => break,
                        }
                    }
                }
            }
        });
    }

    pub async fn shutdown(&self) {
        self.0.shutdown.cancel();
        self.0.tracker.close();
        if let Err(err) = tokio::time::timeout(Duration::from_secs(5), self.0.tracker.wait()).await
        {
            tracing::error!(
                ?err,
                subsystem = self.0.subsystem,
                "Supervisor pool shutdown timed out"
            );
        }
    }

    pub fn is_running(&self, id: i32) -> bool {
        self.0.supervisors.lock().unwrap().contains_key(&id)
    }

    /// Cancel the supervisor for `id`. Returns whether one was present.
    pub fn cancel(&self, id: i32) -> bool {
        if let Some(entry) = self.0.supervisors.lock().unwrap().remove(&id) {
            entry.cancel.cancel();
            return true;
        }
        false
    }

    /// Cancel the supervisors for ids. Same as [`SupervisorPool::cancel`] but for bulk
    pub fn cancel_all(&self, ids: impl IntoIterator<Item = i32>) {
        let mut supervisors = self.0.supervisors.lock().unwrap();
        for id in ids {
            if let Some(entry) = supervisors.remove(&id) {
                entry.cancel.cancel();
            }
        }
    }

    pub async fn nudge(&self) {
        let _ = tokio::time::timeout(Duration::from_millis(500), self.0.refresh_tx.send(())).await;
    }

    pub fn available_capacity(&self) -> usize {
        self.0.semaphore.available_permits()
    }

    // Sync half: reserve the id in the supervisors map, or bail. On success
    // returns the cancel token + a Drop-guard that unwinds the reservation if
    // the caller can't get a permit. Caller must `guard.commit()` on success.
    fn reserve(
        &self,
        id: i32,
        priority: u8,
    ) -> Result<(CancellationToken, super::guard::Guard<impl FnOnce()>), Acquire> {
        let cancel = self.0.shutdown.child_token();
        let mut sup = self.0.supervisors.lock().unwrap();
        if sup.contains_key(&id) {
            return Err(Acquire::AlreadyRunning);
        }
        sup.insert(
            id,
            SlotEntry {
                cancel: cancel.clone(),
                priority,
                started_at: Instant::now(),
            },
        );
        let guard = super::guard::Guard::new(move || {
            self.0.supervisors.lock().unwrap().remove(&id);
            let _ = self.0.refresh_tx.try_send(());
        });

        Ok((cancel, guard))
    }

    fn make_slot(&self, id: i32, cancel: CancellationToken, permit: OwnedSemaphorePermit) -> Slot {
        Slot {
            id,
            permit: Some(permit),
            cancel,
            pool: self.0.clone(),
        }
    }

    pub async fn acquire(&self, id: i32, priority: u8) -> Acquire {
        let (cancel, guard) = match self.reserve(id, priority) {
            Ok(res) => res,
            Err(acquire) => return acquire,
        };

        // See acquire_evicting for explanation why a lock is needed
        let lock = self.0.acquire_evict_mutex.lock().await;

        let Ok(permit) = self.0.semaphore.clone().try_acquire_owned() else {
            return Acquire::NoCapacity;
        };

        guard.commit();
        drop(lock);

        Acquire::Acquired(self.make_slot(id, cancel, permit))
    }

    pub async fn acquire_evicting<Fn, Err>(
        &self,
        id: i32,
        priority: u8,
        on_evict: Fn,
    ) -> Result<Acquire, Err>
    where
        Fn: AsyncFnOnce(i32) -> Result<(), Err>,
    {
        let (cancel, guard) = match self.reserve(id, priority) {
            Ok(res) => res,
            Err(acquire) => return Ok(acquire),
        };

        // This lock prevents other concurrent callers from stealing the permit that we have just freed
        // for ourselves. Without it someone could happily fall through the fast path and get their
        // hands on our permit while we are waiting for `on_evict`
        let lock = self.0.acquire_evict_mutex.lock().await;

        if let Ok(permit) = self.0.semaphore.clone().try_acquire_owned() {
            guard.commit();
            return Ok(Acquire::Acquired(self.make_slot(id, cancel, permit)));
        }

        let Some(victim) = self.find_evictable(priority) else {
            return Ok(Acquire::NoCapacity);
        };

        on_evict(victim).await?;

        let Ok(permit) = self.0.semaphore.clone().acquire_owned().await else {
            return Ok(Acquire::NoCapacity);
        };

        guard.commit();
        drop(lock);

        Ok(Acquire::Acquired(self.make_slot(id, cancel, permit)))
    }

    /// Pick the oldest running supervisor whose priority is **below**
    /// `threshold`, so a higher-priority job can take its slot. Returns
    /// `None` if no entry qualifies. The caller is responsible for actually
    /// evicting it via [`SupervisorPool::cancel`].
    fn find_evictable(&self, threshold: u8) -> Option<i32> {
        self.0
            .supervisors
            .lock()
            .unwrap()
            .iter()
            .filter(|entry| entry.1.priority < threshold)
            .min_by_key(|entry| entry.1.started_at)
            .map(|(key, _)| *key)
    }

    /// Run a helper (e.g. fanned-out `start` calls from `refresh`) on the
    /// pool's tracker so `shutdown` awaits it.
    pub fn spawn_helper<F>(&self, fut: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.0.tracker.spawn(fut);
    }
}
