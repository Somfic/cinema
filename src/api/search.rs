use crate::app::{AppContext, CinemaError};
use crate::tmdb::{SearchResult, TmdbClient};

#[draad::api(namespace = "search")]
pub trait SearchApi {
    /// Full-text search across TMDB titles
    #[get]
    async fn search(&self, q: String) -> Result<Vec<SearchResult>, CinemaError>;
}

#[draad::api]
impl SearchApi for AppContext {
    async fn search(&self, q: String) -> Result<Vec<SearchResult>, CinemaError> {
        let tmdb = TmdbClient::new(&self.config, self.http.clone());
        tmdb.search(&q).await
    }
}
