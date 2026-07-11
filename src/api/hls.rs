use crate::app::AppContext;
use crate::app::Error;

#[draad::api(namespace = "hls")]
pub trait HlsApi {
    /// Stops an HLS transcoding session and tears down its ffmpeg process
    #[delete]
    async fn stop(&self, session_id: String) -> Result<(), Error>;

    /// Current number of live HLS sessions holding a slot in the shared
    /// transcodings semaphore. Used to hydrate the Downloads popover on mount;
    /// steady-state updates come from [`HlsEvents::live_count`].
    #[get]
    async fn live_count(&self) -> Result<usize, Error>;

    /// Stops every live HLS session. Idempotent when no sessions are alive.
    #[post]
    async fn stop_all(&self) -> Result<(), Error>;
}

#[draad::api]
impl HlsApi for AppContext {
    async fn stop(&self, session_id: String) -> Result<(), Error> {
        self.transcodings.stop_live(&session_id).await;
        Ok(())
    }

    async fn live_count(&self) -> Result<usize, Error> {
        Ok(self.transcodings.live_session_count().await)
    }

    async fn stop_all(&self) -> Result<(), Error> {
        self.transcodings.stop_all_live().await;
        Ok(())
    }
}

#[draad::events(namespace = "hls")]
pub trait HlsEvents {
    /// Emitted whenever the count of live HLS sessions changes. Payload is
    /// the new absolute count.
    fn live_count(payload: usize);
}
