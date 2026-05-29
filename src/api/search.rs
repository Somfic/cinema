use crate::app::{AppContext, Error};
pub use crate::tmdb::SearchResult;
use crate::tmdb::TmdbClient;
use cinema_schema::cinema_api;

#[cinema_api(namespace = "search")]
pub trait SearchApi {
    /// Full-text search across TMDB titles
    async fn search(&self, q: String) -> Result<Vec<SearchResult>, Error>;
}

#[cinema_api]
impl SearchApi for AppContext {
    async fn search(&self, q: String) -> Result<Vec<SearchResult>, Error> {
        let tmdb = TmdbClient::new(&self.config, self.http.clone());
        tmdb.search(&q).await
    }
}
