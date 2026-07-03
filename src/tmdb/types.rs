#[draad::ty]
pub struct MediaItem {
    pub id: i32,
    pub tmdb_id: i64,
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
    pub next_episode: Option<NextEpisode>,
}

#[draad::ty]
pub struct NextEpisode {
    pub season_number: i64,
    pub episode_number: i64,
    pub name: String,
    pub air_date: Option<String>,
    pub still_path: Option<String>,
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
#[derive(Copy, PartialEq, Eq, Hash, sqlx::Type)]
#[serde(rename_all = "lowercase")]
#[sqlx(type_name = "media_type", rename_all = "lowercase")]
pub enum MediaType {
    Movie,
    Tv,
}

impl TryFrom<String> for MediaType {
    type Error = crate::app::Error;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        match value.as_str() {
            "movie" => Ok(MediaType::Movie),
            "tv" => Ok(MediaType::Tv),
            _ => Err(crate::app::Error::InvalidInput(format!(
                "Incorrect media type: \"{value}\""
            ))),
        }
    }
}

impl From<MediaType> for &'static str {
    fn from(val: MediaType) -> Self {
        match val {
            MediaType::Movie => "movie",
            MediaType::Tv => "tv",
        }
    }
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
    /// Whether TMDB marks this as an official (studio-published) video.
    pub official: bool,
    /// Max resolution reported by TMDB (e.g. 360, 720, 1080, 2160).
    pub size: i64,
    /// ISO-8601 publish timestamp, used to prefer the most recent trailer.
    pub published_at: Option<String>,
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

impl MediaItem {
    pub(super) async fn upsert_raw<'c, E: sqlx::Executor<'c, Database = sqlx::Postgres>>(
        tmdb_id: i64,
        media_type: MediaType,
        title: &String,
        poster_path: Option<&str>,
        conn: E,
    ) -> crate::app::Result<i32> {
        sqlx::query_scalar!(
            r#"
                INSERT INTO media_items (media_type, tmdb_id, title, poster_path)
                VALUES ($1, $2, $3, $4)
                    ON CONFLICT (media_type, tmdb_id) DO UPDATE SET
                        title = EXCLUDED.title,
                        poster_path = EXCLUDED.poster_path,
                        updated_at = CURRENT_TIMESTAMP
                RETURNING id
            "#,
            media_type as MediaType,
            tmdb_id,
            title,
            poster_path,
        )
        .fetch_one(conn)
        .await
        .map_err(crate::app::Error::DatabaseError)
    }

    pub async fn ensure_exists(
        tmdb_id: i64,
        media_type: MediaType,
        conn: &mut sqlx::PgConnection,
        ctx: &crate::app::AppContext,
    ) -> crate::app::Result<i32> {
        let id = sqlx::query_scalar!(
            "SELECT id FROM media_items where tmdb_id = $1 AND media_type = $2",
            tmdb_id,
            media_type as MediaType
        )
        .fetch_optional(&mut *conn)
        .await
        .map_err(crate::app::Error::DatabaseError)?;

        if let Some(id) = id {
            return Ok(id);
        }

        let client = super::TmdbClient::new(&ctx.config, ctx.http.clone());
        Ok(client.details(media_type, tmdb_id, &mut *conn).await?.id)
    }
}
