use crate::app::{AppContext, Error};

use crate::file_system;

#[draad::api(namespace = "cache")]
pub trait CacheApi {
    // GET
    /// List all media entires that are currenly cached
    async fn items(&self) -> Result<Vec<file_system::CacheEntry>, Error>;

    // DELATE /cache/orphan/{info_hash}
    /// Delete the orphan with the correcponding info_hash
    async fn orphan(&self, info_hash: String) -> Result<(), Error>;

    // POST /cache/clear-app-cache
    /// Wipe app cache; live HLS sessions, pretranscodings, and torrents are not touched.
    async fn clear_app_cache(&self) -> Result<(), Error>;

    // GET
    /// Get the filesystem breakdown
    async fn disk(&self) -> Result<file_system::DiskStats, Error>;
}

#[draad::api]
impl CacheApi for AppContext {
    async fn items(&self) -> Result<Vec<file_system::CacheEntry>, Error> {
        file_system::list_cache_items(self).await
    }

    async fn orphan(&self, info_hash: String) -> Result<(), Error> {
        file_system::delete_cache_orphan(self, info_hash).await
    }

    async fn clear_app_cache(&self) -> Result<(), Error> {
        file_system::clear_app_cache(self).await
    }

    async fn disk(&self) -> Result<file_system::DiskStats, Error> {
        file_system::get_cache_disk(self).await
    }
}
