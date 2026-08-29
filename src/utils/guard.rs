/// RAII guard that executes a cleanup callback when it is dropped
pub struct Guard<F: FnOnce()> {
    committed: bool,
    cleanup: Option<F>,
}

impl<F: FnOnce()> Guard<F> {
    pub fn new(cleanup: F) -> Self {
        Self {
            committed: false,
            cleanup: Some(cleanup),
        }
    }

    /// Commit the guard - cleanup will not be run.
    /// Takes ownership of the guard because after it has been committed,
    /// the guard becomes useless
    pub fn commit(mut self) {
        self.committed = true;
    }
}

impl<F: FnOnce()> Drop for Guard<F> {
    fn drop(&mut self) {
        if !self.committed
            && let Some(cleanup) = self.cleanup.take()
        {
            cleanup();
        }
    }
}
