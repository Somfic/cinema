use crate::app::{AppContext, CinemaError};
use crate::tmdb::{MediaItem, MediaType, SearchResult, TmdbClient};

#[draad::api(namespace = "media")]
pub trait MediaApi {
    /// Full TMDB details for a movie
    #[get]
    async fn movie_details(&self, id: i64) -> Result<MediaItem, CinemaError>;

    /// Full TMDB details for a TV show
    #[get]
    async fn tv_details(&self, id: i64) -> Result<MediaItem, CinemaError>;

    /// Items similar to the given movie/TV id
    #[get]
    async fn similar(&self, media_type: String, id: i64) -> Result<Vec<SearchResult>, CinemaError>;

    /// Currently-trending movies + TV
    #[get]
    async fn trending(&self) -> Result<Vec<SearchResult>, CinemaError>;
}

#[draad::api]
impl MediaApi for AppContext {
    async fn movie_details(&self, id: i64) -> Result<MediaItem, CinemaError> {
        let tmdb = TmdbClient::new(&self.config, self.http.clone());
        tmdb.details(MediaType::Movie, id, &self.db).await
    }

    async fn tv_details(&self, id: i64) -> Result<MediaItem, CinemaError> {
        let tmdb = TmdbClient::new(&self.config, self.http.clone());
        tmdb.details(MediaType::Tv, id, &self.db).await
    }

    async fn similar(&self, media_type: String, id: i64) -> Result<Vec<SearchResult>, CinemaError> {
        let tmdb = TmdbClient::new(&self.config, self.http.clone());
        let mt = MediaType::try_from(media_type)?;
        tmdb.similar(mt, id).await
    }

    async fn trending(&self) -> Result<Vec<SearchResult>, CinemaError> {
        let tmdb = TmdbClient::new(&self.config, self.http.clone());
        tmdb.trending().await
    }
}
