use crate::app::{AppContext, CinemaError};
use crate::trailer::TrailerMeta;

#[draad::api(namespace = "trailer")]
pub trait TrailerApi {
    /// Display metadata (aspect ratio) for a cached trailer, used to size the
    /// player before the video's intrinsic dimensions are known.
    #[get]
    async fn meta(&self, key: String) -> Result<TrailerMeta, CinemaError>;
}

#[draad::api]
impl TrailerApi for AppContext {
    async fn meta(&self, key: String) -> Result<TrailerMeta, CinemaError> {
        crate::trailer::ensure_meta(&self.storage, &key).await
    }
}
