use crate::{
    app::{AppContext, Error},
    downloads::Download,
    file_system,
};

#[draad::ty]
#[serde(rename_all = "lowercase")]
pub enum EntryKind {
    Download,
    Orphan,
}

// TODO: replace strings with enums (reuse enums also in downloads.rs)
#[draad::ty]
pub struct CacheEntry {
    /// "download" for tracked downloads, "orphan" for stray torrent dirs.
    kind: EntryKind,
    /// downloads.id; null for orphans.
    id: Option<i64>,
    info_hash: String,
    title: Option<String>,
    poster_path: Option<String>,
    /// "movie" | "tv" for tracked downloads.
    media_type: Option<String>,
    /// "movies" | "tv" | "orphan" — used by the UI for grouping/filtering.
    category: String,
    season: Option<i64>,
    episode: Option<i64>,
    resolution: Option<String>,
    /// queued | downloading | completed | failed | cancelled for tracked items.
    status: Option<String>,
    /// Actual on-disk size of the torrent directory.
    disk_bytes: u64,
    total_bytes: Option<i64>,
    downloaded_bytes: Option<i64>,
    created_at: Option<String>,
}

pub async fn list_cache_items(ctx: &AppContext) -> Result<Vec<CacheEntry>, Error> {
    let downloads =
        sqlx::query_as::<_, Download>("SELECT * FROM downloads ORDER BY created_at DESC")
            .fetch_all(&ctx.db)
            .await
            .map_err(|e| Error::Generic(e.to_string()))?;

    let torrents = file_system::torrents_root(ctx);
    let mut seen_hashes: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut entries: Vec<CacheEntry> = Vec::with_capacity(downloads.len());

    for dl in downloads {
        let dir = torrents.join(&dl.info_hash);
        let disk_bytes = file_system::dir_size(&dir).await;
        seen_hashes.insert(dl.info_hash.to_lowercase());

        let category = match dl.media_type.as_str() {
            "movie" => "movies",
            "tv" => "tv",
            _ => "other",
        }
        .to_string();

        entries.push(CacheEntry {
            kind: EntryKind::Download,
            id: Some(dl.id),
            info_hash: dl.info_hash,
            title: Some(dl.title),
            poster_path: dl.poster_path,
            media_type: Some(dl.media_type),
            category,
            season: Some(dl.season),
            episode: Some(dl.episode),
            resolution: dl.resolution,
            status: Some(dl.status),
            disk_bytes,
            total_bytes: dl.total_bytes,
            downloaded_bytes: Some(dl.downloaded_bytes),
            created_at: Some(dl.created_at),
        });
    }

    // Find orphan torrent directories (present on disk, no DB row).
    if let Ok(mut rd) = tokio::fs::read_dir(&torrents).await {
        while let Ok(Some(entry)) = rd.next_entry().await {
            let Ok(ft) = entry.file_type().await else {
                continue;
            };
            if !ft.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if seen_hashes.contains(&name.to_lowercase()) {
                continue;
            }
            let disk_bytes = file_system::dir_size(&entry.path()).await;
            entries.push(CacheEntry {
                kind: EntryKind::Orphan,
                id: None,
                info_hash: name,
                title: None,
                poster_path: None,
                media_type: None,
                category: "orphan".into(),
                season: None,
                episode: None,
                resolution: None,
                status: None,
                disk_bytes,
                total_bytes: None,
                downloaded_bytes: None,
                created_at: None,
            });
        }
    }

    Ok(entries)
}

pub async fn delete_cache_orphan(ctx: &AppContext, info_hash: String) -> Result<(), Error> {
    // Reject anything that could escape the torrents root.
    if info_hash.is_empty()
        || info_hash.contains('/')
        || info_hash.contains('\\')
        || info_hash.contains("..")
    {
        return Err(Error::InvalidInput("invalid info_hash".into()));
    }

    let root = file_system::torrents_root(ctx);
    let target = root.join(&info_hash);

    // Confirm the resolved path is inside the torrents root.
    let canon_root = tokio::fs::canonicalize(&root).await.ok();
    let canon_target = tokio::fs::canonicalize(&target).await.ok();
    if let (Some(r), Some(t)) = (canon_root, canon_target)
        && !t.starts_with(&r)
    {
        return Err(Error::InvalidInput("invalid info_hash".into()));
    }

    if target.exists() {
        tokio::fs::remove_dir_all(&target).await?;
    }

    Ok(())
}

pub async fn clear_app_cache(ctx: &AppContext) -> Result<(), Error> {
    crate::hls::stop_all().await;

    // Wipe every subdirectory of data_dir/fs/ except `torrents/` (downloads).
    let root = ctx.storage.path().to_path_buf();
    if let Ok(mut rd) = tokio::fs::read_dir(&root).await {
        while let Ok(Some(entry)) = rd.next_entry().await {
            let name = entry.file_name();
            if name == std::ffi::OsStr::new("torrents") {
                continue;
            }
            let Ok(ft) = entry.file_type().await else {
                continue;
            };
            let path = entry.path();
            if ft.is_dir() {
                let _ = tokio::fs::remove_dir_all(&path).await;
            } else if ft.is_file() {
                let _ = tokio::fs::remove_file(&path).await;
            }
        }
    }

    Ok(())
}
