use crate::app::AppContext;
pub use crate::app::Error;
pub use crate::tmdb::SearchResult;
use crate::tmdb::TmdbClient;

#[draad::api(namespace = "search")]
pub trait SearchApi {
    /// Full-text search across TMDB titles
    #[get]
    async fn search(&self, q: String) -> Result<Vec<SearchResult>, Error>;
}

#[draad::api]
impl SearchApi for AppContext {
    async fn search(&self, q: String) -> Result<Vec<SearchResult>, Error> {
        let tmdb = TmdbClient::new(&self.config, self.http.clone());
        tmdb.search(&q).await
    }
}
