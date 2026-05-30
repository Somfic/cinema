use serde::Deserialize;

use crate::config::Config;

#[draad::ty]
pub struct MediaItem {
    pub id: i64,
    pub imdb_id: Option<String>,
    pub media_type: MediaType,
    pub title: String,
    pub overview: Option<String>,
    pub tagline: Option<String>,
    pub release_date: Option<String>,
    pub runtime: Option<i64>,
    pub rating: Option<f64>,
    pub poster_path: Option<String>,
    pub backdrops: Vec<String>,
    pub genres: Vec<Genre>,
    pub videos: Vec<Video>,
    pub logo_path: Option<String>,
    pub seasons: Option<Vec<Season>>,
    pub cast: Vec<CastMember>,
    pub directors: Vec<CrewMember>,
}

#[draad::ty]
pub struct CastMember {
    pub id: i64,
    pub name: String,
    pub character: Option<String>,
    pub profile_path: Option<String>,
}

#[draad::ty]
pub struct CrewMember {
    pub id: i64,
    pub name: String,
    pub job: String,
    pub profile_path: Option<String>,
}

#[draad::ty]
#[derive(Copy, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum MediaType {
    Movie,
    Tv,
}

#[draad::ty]
pub struct SearchResult {
    pub id: i64,
    pub media_type: MediaType,
    pub title: String,
    pub overview: Option<String>,
    pub release_date: Option<String>,
    pub poster_path: Option<String>,
    pub backdrop_path: Option<String>,
}

#[draad::ty]
pub struct Genre {
    pub id: i64,
    pub name: String,
}

#[draad::ty]
pub struct Video {
    pub key: String,
    pub site: String,
    pub name: String,
    pub video_type: String,
}

#[draad::ty]
pub struct Season {
    pub id: i64,
    pub season_number: i64,
    pub name: String,
    pub episode_count: i64,
    pub poster_path: Option<String>,
    pub air_date: Option<String>,
    pub episodes: Vec<Episode>,
}

#[draad::ty]
pub struct Episode {
    pub episode_number: i64,
    pub name: String,
    pub overview: Option<String>,
    pub stills: Vec<String>,
}

// --- Raw TMDB response types (private) ---

#[derive(Deserialize)]
struct TmdbMultiSearchResults {
    results: Vec<TmdbMultiSearchResult>,
}

#[derive(Deserialize)]
struct TmdbMultiSearchResult {
    id: i64,
    #[serde(default)]
    media_type: Option<String>,
    // movie fields
    title: Option<String>,
    release_date: Option<String>,
    // tv fields
    name: Option<String>,
    first_air_date: Option<String>,
    // shared
    overview: Option<String>,
    poster_path: Option<String>,
    backdrop_path: Option<String>,
}

impl TmdbMultiSearchResult {
    fn into_search_result(self, fallback_type: Option<MediaType>) -> Option<SearchResult> {
        let media_type = match self.media_type.as_deref() {
            Some("movie") => MediaType::Movie,
            Some("tv") => MediaType::Tv,
            _ => fallback_type?,
        };
        Some(SearchResult {
            id: self.id,
            media_type,
            title: self.title.or(self.name).unwrap_or_default(),
            overview: self.overview,
            release_date: self.release_date.or(self.first_air_date),
            poster_path: self.poster_path,
            backdrop_path: self.backdrop_path,
        })
    }
}

#[derive(Deserialize)]
struct TmdbMovie {
    id: i64,
    imdb_id: Option<String>,
    title: String,
    overview: Option<String>,
    tagline: Option<String>,
    release_date: Option<String>,
    runtime: Option<i64>,
    vote_average: Option<f64>,
    poster_path: Option<String>,
    backdrop_path: Option<String>,
    genres: Vec<Genre>,
    videos: Option<TmdbVideos>,
    images: Option<TmdbImages>,
    credits: Option<TmdbCredits>,
}

#[derive(Deserialize)]
struct TmdbTv {
    id: i64,
    name: String,
    overview: Option<String>,
    tagline: Option<String>,
    first_air_date: Option<String>,
    episode_run_time: Option<Vec<i64>>,
    vote_average: Option<f64>,
    poster_path: Option<String>,
    backdrop_path: Option<String>,
    genres: Vec<Genre>,
    videos: Option<TmdbVideos>,
    images: Option<TmdbImages>,
    seasons: Option<Vec<TmdbSeason>>,
    external_ids: Option<TmdbExternalIds>,
    credits: Option<TmdbCredits>,
}

#[derive(Deserialize)]
struct TmdbCredits {
    #[serde(default)]
    cast: Vec<TmdbCastMember>,
    #[serde(default)]
    crew: Vec<TmdbCrewMember>,
}

#[derive(Deserialize)]
struct TmdbCastMember {
    id: i64,
    name: String,
    character: Option<String>,
    profile_path: Option<String>,
}

#[derive(Deserialize)]
struct TmdbCrewMember {
    id: i64,
    name: String,
    job: String,
    profile_path: Option<String>,
}

#[derive(Deserialize)]
struct TmdbExternalIds {
    imdb_id: Option<String>,
}

#[derive(Deserialize)]
struct TmdbSeason {
    id: i64,
    season_number: i64,
    name: String,
    episode_count: i64,
    poster_path: Option<String>,
    air_date: Option<String>,
}

#[derive(Deserialize)]
struct TmdbSeasonDetail {
    episodes: Vec<TmdbEpisode>,
}

#[derive(Deserialize)]
struct TmdbEpisodeImages {
    stills: Vec<TmdbImage>,
}

#[derive(Deserialize)]
struct TmdbEpisode {
    episode_number: i64,
    name: String,
    overview: Option<String>,
    still_path: Option<String>,
}

#[derive(Deserialize)]
struct TmdbVideos {
    results: Vec<TmdbVideo>,
}

#[derive(Deserialize)]
struct TmdbVideo {
    key: String,
    site: String,
    name: String,
    #[serde(rename = "type")]
    video_type: String,
}

#[derive(Deserialize)]
struct TmdbImages {
    posters: Vec<TmdbImage>,
    backdrops: Vec<TmdbImage>,
    logos: Vec<TmdbImage>,
}

#[derive(Deserialize)]
struct TmdbImage {
    file_path: String,
    width: i64,
    height: i64,
    iso_639_1: Option<String>,
    vote_average: f64,
}

// --- Conversions ---

fn convert_videos(videos: Option<TmdbVideos>) -> Vec<Video> {
    videos
        .map(|v| {
            v.results
                .into_iter()
                .map(|v| Video {
                    key: v.key,
                    site: v.site,
                    name: v.name,
                    video_type: v.video_type,
                })
                .collect()
        })
        .unwrap_or_default()
}

fn convert_cast(credits: &Option<TmdbCredits>) -> Vec<CastMember> {
    credits
        .as_ref()
        .map(|c| {
            c.cast
                .iter()
                .take(20)
                .map(|m| CastMember {
                    id: m.id,
                    name: m.name.clone(),
                    character: m.character.clone(),
                    profile_path: m.profile_path.clone(),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn convert_directors(credits: &Option<TmdbCredits>) -> Vec<CrewMember> {
    credits
        .as_ref()
        .map(|c| {
            c.crew
                .iter()
                .filter(|m| m.job == "Director")
                .map(|m| CrewMember {
                    id: m.id,
                    name: m.name.clone(),
                    job: m.job.clone(),
                    profile_path: m.profile_path.clone(),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Pick the best poster: prefer English, then no-language, then highest resolution
fn pick_poster(images: &Option<TmdbImages>, fallback: Option<&str>) -> Option<String> {
    if let Some(imgs) = images {
        let mut posters: Vec<&TmdbImage> = imgs.posters.iter().collect();
        if !posters.is_empty() {
            posters.sort_by(|a, b| {
                let a_en = (a.iso_639_1.as_deref() == Some("en")) as u8;
                let b_en = (b.iso_639_1.as_deref() == Some("en")) as u8;
                let a_clean = a.iso_639_1.is_none() as u8;
                let b_clean = b.iso_639_1.is_none() as u8;
                b_en.cmp(&a_en)
                    .then(b_clean.cmp(&a_clean))
                    .then(b.width.cmp(&a.width))
            });
            return Some(posters[0].file_path.clone());
        }
    }
    fallback.map(|s| s.to_string())
}

/// Pick backdrops: filter ≥1080p, deduplicate, weighted shuffle by votes
fn pick_backdrops(images: &Option<TmdbImages>, fallback: Option<&str>) -> Vec<String> {
    let mut paths: Vec<String> = Vec::new();
    if let Some(imgs) = images {
        use std::collections::HashSet;

        // Filter: ≥1080p width, no-text preferred, deduplicate
        let mut seen = HashSet::new();
        let mut candidates: Vec<&TmdbImage> = imgs
            .backdrops
            .iter()
            .filter(|b| b.width >= 1920)
            .filter(|b| b.iso_639_1.is_none()) // No text overlays
            .filter(|b| seen.insert(&b.file_path))
            .collect();

        // Sort by votes descending — first element is always the most upvoted
        candidates.sort_by(|a, b| {
            b.vote_average
                .partial_cmp(&a.vote_average)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // Shuffle the rest (index 1+) with weighted randomness
        if candidates.len() > 1 {
            use std::hash::{Hash, Hasher};
            let seed = {
                let mut h = std::hash::DefaultHasher::new();
                std::time::SystemTime::now().hash(&mut h);
                h.finish()
            };
            candidates[1..].sort_by(|a, b| {
                let wa = a.vote_average + pseudo_rand(seed, &a.file_path) * 2.0;
                let wb = b.vote_average + pseudo_rand(seed, &b.file_path) * 2.0;
                wb.partial_cmp(&wa).unwrap_or(std::cmp::Ordering::Equal)
            });
        }

        paths.extend(candidates.iter().map(|b| b.file_path.clone()));
    }
    // If images had no backdrops, use the main backdrop_path as fallback
    if paths.is_empty() {
        if let Some(p) = fallback {
            paths.push(p.to_string());
        }
    }
    paths
}

/// Deterministic pseudo-random value 0..1 from a seed and a string key
fn pseudo_rand(seed: u64, key: &str) -> f64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::hash::DefaultHasher::new();
    seed.hash(&mut h);
    key.hash(&mut h);
    (h.finish() % 10000) as f64 / 10000.0
}

/// Pick the best logo: highest resolution English one, fallback to highest resolution overall
fn pick_logo(images: &Option<TmdbImages>) -> Option<String> {
    let imgs = images.as_ref()?;
    let mut logos: Vec<&TmdbImage> = imgs.logos.iter().collect();
    if logos.is_empty() {
        return None;
    }
    logos.sort_by(|a, b| b.width.cmp(&a.width));
    logos
        .iter()
        .find(|l| l.iso_639_1.as_deref() == Some("en"))
        .or_else(|| logos.first())
        .map(|l| l.file_path.clone())
}

impl From<TmdbMovie> for MediaItem {
    fn from(m: TmdbMovie) -> Self {
        MediaItem {
            id: m.id,
            imdb_id: m.imdb_id,
            media_type: MediaType::Movie,
            title: m.title,
            overview: m.overview,
            tagline: m.tagline.map(|t| t.trim_end_matches('.').to_string()),
            release_date: m.release_date,
            runtime: m.runtime,
            rating: m.vote_average,
            poster_path: pick_poster(&m.images, m.poster_path.as_deref()),
            backdrops: pick_backdrops(&m.images, m.backdrop_path.as_deref()),
            genres: m.genres,
            videos: convert_videos(m.videos),
            logo_path: pick_logo(&m.images),
            seasons: None,
            cast: convert_cast(&m.credits),
            directors: convert_directors(&m.credits),
        }
    }
}

impl From<TmdbTv> for MediaItem {
    fn from(t: TmdbTv) -> Self {
        MediaItem {
            id: t.id,
            imdb_id: t.external_ids.and_then(|e| e.imdb_id),
            media_type: MediaType::Tv,
            title: t.name,
            overview: t.overview,
            tagline: t.tagline.map(|t| t.trim_end_matches('.').to_string()),
            release_date: t.first_air_date,
            runtime: t.episode_run_time.and_then(|r| r.first().copied()),
            rating: t.vote_average,
            poster_path: pick_poster(&t.images, t.poster_path.as_deref()),
            backdrops: pick_backdrops(&t.images, t.backdrop_path.as_deref()),
            genres: t.genres,
            videos: convert_videos(t.videos),
            logo_path: pick_logo(&t.images),
            seasons: t.seasons.map(|s| {
                s.into_iter()
                    .filter(|s| s.season_number > 0) // Exclude "Specials" (season 0)
                    .map(|s| Season {
                        id: s.id,
                        season_number: s.season_number,
                        name: s.name,
                        episode_count: s.episode_count,
                        poster_path: s.poster_path,
                        air_date: s.air_date,
                        episodes: vec![],
                    })
                    .collect()
            }),
            cast: convert_cast(&t.credits),
            directors: convert_directors(&t.credits),
        }
    }
}

// --- Client ---

pub struct TmdbClient {
    api_key: String,
    client: reqwest::Client,
}

impl TmdbClient {
    pub fn new(config: &Config, client: reqwest::Client) -> Self {
        Self {
            api_key: config.tmdb_api_key.clone(),
            client,
        }
    }

    pub async fn ping(&self) -> crate::app::Result<String> {
        let url = format!(
            "https://api.themoviedb.org/3/authentication?api_key={}",
            self.api_key
        );
        let res = self.client.get(&url).send().await?;
        let status = res.status();
        if status.is_success() {
            Ok("authenticated".into())
        } else {
            Err(crate::app::Error::Generic(status.to_string()))
        }
    }

    pub async fn search(&self, query: &str) -> crate::app::Result<Vec<SearchResult>> {
        // Normalize hyphens to periods (e.g. "wall-e" → "wall.e" matches "WALL·E")
        let query = query.replace('-', ".");
        let encoded = urlencoding::encode(&query);
        let multi_url = format!(
            "https://api.themoviedb.org/3/search/multi?api_key={}&query={}",
            self.api_key, encoded
        );
        let movie_url = format!(
            "https://api.themoviedb.org/3/search/movie?api_key={}&query={}",
            self.api_key, encoded
        );
        let tv_url = format!(
            "https://api.themoviedb.org/3/search/tv?api_key={}&query={}",
            self.api_key, encoded
        );

        let (multi, movies, tv) = futures::future::join3(
            self.client.get(&multi_url).send(),
            self.client.get(&movie_url).send(),
            self.client.get(&tv_url).send(),
        )
        .await;

        let mut seen = std::collections::HashSet::new();
        let mut results = Vec::new();

        // Multi-search results first (primary)
        if let Ok(res) = multi {
            if let Ok(body) = res.text().await {
                if let Ok(data) = serde_json::from_str::<TmdbMultiSearchResults>(&body) {
                    for r in data.results {
                        if let Some(sr) = r.into_search_result(None) {
                            seen.insert((sr.media_type.clone(), sr.id));
                            results.push(sr);
                        }
                    }
                }
            }
        }

        // Movie-specific results (catches things multi-search misses)
        if let Ok(res) = movies {
            if let Ok(body) = res.text().await {
                if let Ok(data) = serde_json::from_str::<TmdbMultiSearchResults>(&body) {
                    for r in data.results {
                        if let Some(sr) = r.into_search_result(Some(MediaType::Movie)) {
                            if seen.insert((sr.media_type.clone(), sr.id)) {
                                results.push(sr);
                            }
                        }
                    }
                }
            }
        }

        // TV-specific results
        if let Ok(res) = tv {
            if let Ok(body) = res.text().await {
                if let Ok(data) = serde_json::from_str::<TmdbMultiSearchResults>(&body) {
                    for r in data.results {
                        if let Some(sr) = r.into_search_result(Some(MediaType::Tv)) {
                            if seen.insert((sr.media_type.clone(), sr.id)) {
                                results.push(sr);
                            }
                        }
                    }
                }
            }
        }

        Ok(results)
    }

    pub async fn details(&self, media_type: MediaType, id: i64) -> crate::app::Result<MediaItem> {
        let type_str = match media_type {
            MediaType::Movie => "movie",
            MediaType::Tv => "tv",
        };
        let url = format!(
            "https://api.themoviedb.org/3/{}/{}?api_key={}&append_to_response=videos,images,external_ids,credits&include_image_language=en,null",
            type_str, id, self.api_key
        );
        let res = self.client.get(&url).send().await?.error_for_status()?;
        let body = res.text().await?;

        let mut item: MediaItem = match media_type {
            MediaType::Movie => serde_json::from_str::<TmdbMovie>(&body)?.into(),
            MediaType::Tv => serde_json::from_str::<TmdbTv>(&body)?.into(),
        };

        // Fetch episode details for each season in parallel
        if media_type == MediaType::Tv {
            if let Some(ref mut seasons) = item.seasons {
                let futs: Vec<_> = seasons
                    .iter()
                    .map(|s| self.fetch_season_episodes(id, s.season_number))
                    .collect();
                let results = futures::future::join_all(futs).await;
                for (season, episodes) in seasons.iter_mut().zip(results) {
                    season.episodes = episodes.unwrap_or_default();
                }
            }
        }

        Ok(item)
    }

    async fn fetch_season_episodes(
        &self,
        tv_id: i64,
        season_number: i64,
    ) -> crate::app::Result<Vec<Episode>> {
        let url = format!(
            "https://api.themoviedb.org/3/tv/{}/season/{}?api_key={}",
            tv_id, season_number, self.api_key
        );
        let res = self.client.get(&url).send().await?.error_for_status()?;
        let body = res.text().await?;
        let detail: TmdbSeasonDetail = serde_json::from_str(&body)?;

        // Fetch episode images in parallel
        let futs: Vec<_> = detail
            .episodes
            .iter()
            .map(|e| self.fetch_episode_stills(tv_id, season_number, e.episode_number))
            .collect();
        let stills_results = futures::future::join_all(futs).await;

        Ok(detail
            .episodes
            .into_iter()
            .zip(stills_results)
            .map(|(e, stills)| {
                let mut stills = stills.unwrap_or_default();
                // Use the default still_path as fallback if images endpoint returned nothing
                if stills.is_empty() {
                    if let Some(ref path) = e.still_path {
                        stills.push(path.clone());
                    }
                }
                Episode {
                    episode_number: e.episode_number,
                    name: e.name,
                    overview: e.overview,
                    stills,
                }
            })
            .collect())
    }

    async fn fetch_episode_stills(
        &self,
        tv_id: i64,
        season_number: i64,
        episode_number: i64,
    ) -> crate::app::Result<Vec<String>> {
        let url = format!(
            "https://api.themoviedb.org/3/tv/{}/season/{}/episode/{}/images?api_key={}&include_image_language=en,null",
            tv_id, season_number, episode_number, self.api_key
        );
        let res = self.client.get(&url).send().await?.error_for_status()?;
        let body = res.text().await?;
        let imgs: TmdbEpisodeImages = serde_json::from_str(&body)?;

        let mut candidates: Vec<&TmdbImage> = imgs
            .stills
            .iter()
            .filter(|s| s.width >= 1280)
            .filter(|s| s.iso_639_1.is_none())
            .collect();

        candidates.sort_by(|a, b| {
            b.vote_average
                .partial_cmp(&a.vote_average)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        Ok(candidates.iter().map(|s| s.file_path.clone()).collect())
    }

    pub async fn trending(&self) -> crate::app::Result<Vec<SearchResult>> {
        let url = format!(
            "https://api.themoviedb.org/3/trending/all/week?api_key={}",
            self.api_key
        );
        let res = self.client.get(&url).send().await?.error_for_status()?;
        let body = res.text().await?;
        let data: TmdbMultiSearchResults = serde_json::from_str(&body)?;
        Ok(data
            .results
            .into_iter()
            .filter_map(|r| r.into_search_result(None))
            .collect())
    }

    pub async fn similar(
        &self,
        media_type: MediaType,
        id: i64,
    ) -> crate::app::Result<Vec<SearchResult>> {
        let type_str = match media_type {
            MediaType::Movie => "movie",
            MediaType::Tv => "tv",
        };
        let url = format!(
            "https://api.themoviedb.org/3/{}/{}/similar?api_key={}",
            type_str, id, self.api_key
        );
        let res = self.client.get(&url).send().await?.error_for_status()?;
        let body = res.text().await?;
        let data: TmdbMultiSearchResults = serde_json::from_str(&body)?;
        Ok(data
            .results
            .into_iter()
            .filter_map(|r| r.into_search_result(Some(media_type)))
            .collect())
    }
}
