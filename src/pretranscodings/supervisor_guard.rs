pub struct SupervisorGuard<F: FnOnce()> {
    committed: bool,
    cleanup: Option<F>,
}

impl<F: FnOnce()> SupervisorGuard<F> {
    pub fn new(cleanup: F) -> Self {
        Self {
            committed: false,
            cleanup: Some(cleanup),
        }
    }

    pub fn commit(mut self) {
        self.committed = true;
    }
}

impl<F: FnOnce()> Drop for SupervisorGuard<F> {
    fn drop(&mut self) {
        if !self.committed
            && let Some(cleanup) = self.cleanup.take()
        {
            cleanup();
        }
    }
}
