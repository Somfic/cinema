use crate::app::{AppContext, CinemaError};
use crate::subtitles as subtitles_mod;
use crate::tmdb::{MediaType, TmdbClient};

pub use crate::subtitles::{SubtitleCue, SubtitleTrack};

#[draad::api(namespace = "subtitles")]
pub trait SubtitlesApi {
    /// External subtitle tracks (OpenSubtitles etc.) for a movie
    #[get]
    async fn movie(&self, id: i64) -> Result<Vec<SubtitleTrack>, CinemaError>;

    /// External subtitle tracks for a specific TV episode
    #[get]
    async fn tv(
        &self,
        id: i64,
        season: i64,
        episode: i64,
    ) -> Result<Vec<SubtitleTrack>, CinemaError>;

    /// Fetches and parses cues from an SRT URL
    #[get]
    async fn cues(&self, url: String) -> Result<Vec<SubtitleCue>, CinemaError>;
}

#[draad::api]
impl SubtitlesApi for AppContext {
    async fn movie(&self, id: i64) -> Result<Vec<SubtitleTrack>, CinemaError> {
        let tmdb = TmdbClient::new(&self.config, self.http.clone());
        let item = tmdb.details(MediaType::Movie, id, &self.db).await?;
        let imdb_id = item
            .imdb_id
            .ok_or_else(|| CinemaError::Generic("No IMDB ID found".into()))?;
        let path = format!("movie/{imdb_id}");
        Ok(subtitles_mod::fetch_tracks(&self.http, &path, &self.config.subtitle_languages).await)
    }

    async fn tv(
        &self,
        id: i64,
        season: i64,
        episode: i64,
    ) -> Result<Vec<SubtitleTrack>, CinemaError> {
        let tmdb = TmdbClient::new(&self.config, self.http.clone());
        let item = tmdb.details(MediaType::Tv, id, &self.db).await?;
        let imdb_id = item
            .imdb_id
            .ok_or_else(|| CinemaError::Generic("No IMDB ID found".into()))?;
        let path = format!("series/{imdb_id}:{season}:{episode}");
        Ok(subtitles_mod::fetch_tracks(&self.http, &path, &self.config.subtitle_languages).await)
    }

    async fn cues(&self, url: String) -> Result<Vec<SubtitleCue>, CinemaError> {
        Ok(subtitles_mod::fetch_cues(&self.http, &url).await)
    }
}
