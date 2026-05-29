use cinema_schema::cinema_api;

use crate::app::{AppContext, Error};

#[cinema_api(namespace = "hls")]
pub trait HlsApi {
    /// Stops an HLS transcoding session and tears down its ffmpeg process
    async fn stop(&self, session_id: String) -> Result<(), Error>;
}

#[cinema_api]
impl HlsApi for AppContext {
    async fn stop(&self, session_id: String) -> Result<(), Error> {
        crate::hls::stop_session(&session_id).await;
        Ok(())
    }
}
