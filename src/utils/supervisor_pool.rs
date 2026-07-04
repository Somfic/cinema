//! Shared concurrency primitives for supervised job pools (downloads,
//! pretranscodings). Owns capacity, per-id cancellation, the task tracker,
//! and the refresh nudge loop; managers layer domain logic on top.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

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
            if let Some(cancel) = supervisors.remove(&self.id) {
                cancel.cancel();
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

#[derive(Clone)]
pub struct SupervisorPool(Arc<PoolInner>);

struct PoolInner {
    subsystem: &'static str,
    semaphore: Arc<Semaphore>,
    supervisors: Mutex<HashMap<i32, CancellationToken>>,
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
        if let Some(cancel) = self.0.supervisors.lock().unwrap().remove(&id) {
            cancel.cancel();
            return true;
        }
        false
    }

    /// Cancel the supervisors for ids. Same as [`SupervisorPool::cancel`] but for bulk
    pub fn cancel_all(&self, ids: impl IntoIterator<Item = i32>) {
        let mut supervisors = self.0.supervisors.lock().unwrap();
        for id in ids {
            if let Some(cancel) = supervisors.remove(&id) {
                cancel.cancel();
            }
        }
    }

    pub async fn nudge(&self) {
        let _ = tokio::time::timeout(Duration::from_millis(500), self.0.refresh_tx.send(())).await;
    }

    pub fn available_capacity(&self) -> usize {
        self.0.semaphore.available_permits()
    }

    pub fn try_acquire(&self, id: i32) -> Acquire {
        if self.is_running(id) {
            return Acquire::AlreadyRunning;
        }

        let cancel = self.0.shutdown.child_token();
        let guard = {
            let mut sup = self.0.supervisors.lock().unwrap();
            if sup.contains_key(&id) {
                return Acquire::AlreadyRunning;
            }
            sup.insert(id, cancel.clone());
            super::guard::Guard::new(|| {
                self.0.supervisors.lock().unwrap().remove(&id);
                let _ = self.0.refresh_tx.try_send(());
            })
        };

        let permit = match self.0.semaphore.clone().try_acquire_owned() {
            Ok(p) => p,
            Err(_) => return Acquire::NoCapacity,
        };

        guard.commit();

        Acquire::Acquired(Slot {
            id,
            permit: Some(permit),
            cancel,
            pool: self.0.clone(),
        })
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
