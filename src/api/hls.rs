use crate::app::AppContext;
use crate::app::Error;

#[draad::api(namespace = "hls")]
pub trait HlsApi {
    /// Stops an HLS transcoding session and tears down its ffmpeg process
    #[delete]
    async fn stop(&self, session_id: String) -> Result<(), Error>;
}

#[draad::api]
impl HlsApi for AppContext {
    async fn stop(&self, session_id: String) -> Result<(), Error> {
        self.transcodings.stop_live(&session_id).await;
        Ok(())
    }
}
