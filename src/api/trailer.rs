use crate::app::{AppContext, Error};
use crate::trailer::TrailerMeta;

#[draad::api(namespace = "trailer")]
pub trait TrailerApi {
    /// Display metadata (aspect ratio) for a cached trailer, used to size the
    /// player before the video's intrinsic dimensions are known.
    #[get]
    async fn meta(&self, key: String) -> Result<TrailerMeta, Error>;
}

#[draad::api]
impl TrailerApi for AppContext {
    async fn meta(&self, key: String) -> Result<TrailerMeta, Error> {
        crate::trailer::ensure_meta(&self.storage, &key).await
    }
}
