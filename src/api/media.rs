use crate::app::AppContext;
pub use crate::app::Error;
use crate::tmdb::{MediaType, TmdbClient};

pub use crate::tmdb::{MediaItem, SearchResult};

#[draad::api(namespace = "media")]
pub trait MediaApi {
    /// Full TMDB details for a movie
    #[get]
    async fn movie_details(&self, id: i64) -> Result<MediaItem, Error>;

    /// Full TMDB details for a TV show
    #[get]
    async fn tv_details(&self, id: i64) -> Result<MediaItem, Error>;

    /// Items similar to the given movie/TV id
    #[get]
    async fn similar(&self, media_type: String, id: i64) -> Result<Vec<SearchResult>, Error>;

    /// Currently-trending movies + TV
    #[get]
    async fn trending(&self) -> Result<Vec<SearchResult>, Error>;
}

#[draad::api]
impl MediaApi for AppContext {
    async fn movie_details(&self, id: i64) -> Result<MediaItem, Error> {
        let tmdb = TmdbClient::new(&self.config, self.http.clone());
        tmdb.details(MediaType::Movie, id).await
    }

    async fn tv_details(&self, id: i64) -> Result<MediaItem, Error> {
        let tmdb = TmdbClient::new(&self.config, self.http.clone());
        tmdb.details(MediaType::Tv, id).await
    }

    async fn similar(&self, media_type: String, id: i64) -> Result<Vec<SearchResult>, Error> {
        let tmdb = TmdbClient::new(&self.config, self.http.clone());
        let mt = MediaType::try_from(media_type)?;
        tmdb.similar(mt, id).await
    }

    async fn trending(&self) -> Result<Vec<SearchResult>, Error> {
        let tmdb = TmdbClient::new(&self.config, self.http.clone());
        tmdb.trending().await
    }
}
