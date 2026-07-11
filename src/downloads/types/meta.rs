use crate::tmdb;

#[draad::ty]
pub struct DownloadMeta {
    pub media_item: Option<DownloadMetaMediaItem>,
    pub season: Option<i32>,
    pub episode: Option<i32>,
    pub resolution: Option<String>,
}

#[draad::ty]
pub struct DownloadMetaMediaItem {
    pub tmdb_id: i64,
    pub media_type: tmdb::MediaType,
    pub title: String,
    pub poster_path: Option<String>,
}

pub(crate) struct DownloadMetaContext<'a> {
    pub tmdb_id: i64,
    pub media_type: tmdb::MediaType,
    pub rows: Vec<DownloadMetaRow<'a>>,
}

pub(crate) struct DownloadMetaRow<'a> {
    pub info_hash: &'a str,
    pub file_idx: i32,
    pub season: Option<i32>,
    pub episode: Option<i32>,
    pub resolution: Option<&'a String>,
}

impl DownloadMeta {
    /// Upsert media_items + download_meta linking media context to a download.
    pub async fn upsert(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        meta_ctx: DownloadMetaContext<'_>,
        ctx: &crate::app::AppContext,
    ) -> crate::app::Result<()> {
        let media_id =
            tmdb::MediaItem::ensure_exists(meta_ctx.tmdb_id, meta_ctx.media_type, tx, ctx).await?;

        let info_hashes: Vec<&str> = meta_ctx.rows.iter().map(|r| r.info_hash).collect();
        let file_idxs: Vec<i32> = meta_ctx.rows.iter().map(|r| r.file_idx).collect();
        let media_ids: Vec<i32> = meta_ctx.rows.iter().map(|_| media_id).collect();
        let seasons: Vec<Option<i32>> = meta_ctx.rows.iter().map(|r| r.season).collect();
        let episodes: Vec<Option<i32>> = meta_ctx.rows.iter().map(|r| r.episode).collect();
        let resolutions: Vec<Option<&String>> =
            meta_ctx.rows.iter().map(|r| r.resolution).collect();

        sqlx::query!(
            r#"
                INSERT INTO download_meta (info_hash, file_idx, media_id, season, episode, resolution)
                SELECT * FROM UNNEST($1::text[], $2::int4[], $3::int4[], $4::int4[], $5::int4[], $6::text[])
                ON CONFLICT (info_hash, file_idx) DO UPDATE SET
                    media_id = EXCLUDED.media_id,
                    season = EXCLUDED.season,
                    episode = EXCLUDED.episode,
                    resolution = EXCLUDED.resolution
            "#,
            &info_hashes as &[&str],
            &file_idxs as &[i32],
            &media_ids as &[i32],
            &seasons as &[Option<i32>],
            &episodes as &[Option<i32>],
            &resolutions as &[Option<&String>],
        )
        .execute(&mut **tx)
        .await
        .map_err(crate::app::Error::DatabaseError)?;

        Ok(())
    }
}
