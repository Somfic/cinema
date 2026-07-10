use crate::{
    app::{AppContext, Error},
    file_system,
    transcodings::{PretranscodingOutputPath, types::Pretranscoding},
};

#[draad::ty]
#[serde(rename_all = "lowercase")]
pub enum EntryKind {
    Download,
    Pretranscoding,
    Orphan,
}

#[draad::ty]
pub struct CacheEntry {
    /// "download" for tracked downloads, "pretranscoding" for background
    /// transcoding artifacts, "orphan" for stray torrent dirs.
    kind: EntryKind,
    /// Parent download's info_hash. For orphans this is the on-disk dir name.
    info_hash: String,
    /// Corresponding download row. `None` for orphans and pretranscodings.
    download: Option<crate::downloads::types::Download>,
    /// Corresponding pretranscoding row. `None` unless `kind == Pretranscoding`.
    pretranscoding: Option<Pretranscoding>,
    /// Actual on-disk size of the underlying files.
    disk_bytes: u64,
}

pub async fn list_cache_items(ctx: &AppContext) -> Result<Vec<CacheEntry>, Error> {
    let downloads = crate::downloads::types::Download::find_all(&ctx.db).await?;

    let torrents = ctx.storage.torrents_dir();
    let mut seen_hashes: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut entries: Vec<CacheEntry> = Vec::with_capacity(downloads.len());

    for download in downloads {
        let dir = download.output_path(&ctx.storage);
        let disk_bytes = file_system::dir_size(&dir).await;
        seen_hashes.insert(download.info_hash.to_lowercase());

        entries.push(CacheEntry {
            kind: EntryKind::Download,
            info_hash: download.info_hash.clone(),
            download: Some(download),
            pretranscoding: None,
            disk_bytes,
        });
    }

    for pretransoding in Pretranscoding::find_all(&ctx.db).await? {
        let path = PretranscodingOutputPath::new(
            &ctx.storage,
            pretransoding.download_id,
            pretransoding.only_audio,
            pretransoding.audio_index,
        );
        let disk_bytes = path.disk_bytes().await;
        entries.push(CacheEntry {
            kind: EntryKind::Pretranscoding,
            info_hash: pretransoding.download_info_hash.clone(),
            download: None,
            pretranscoding: Some(pretransoding),
            disk_bytes,
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
                info_hash: name,
                download: None,
                pretranscoding: None,
                disk_bytes,
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

    let root = ctx.storage.torrents_dir();
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

/// Wipe the contents of `data_dir/fs/cache/` (trailers and image thumbnails).
/// Live HLS sessions, pretranscodings, and torrents are intentionally left alone.
pub async fn clear_app_cache(ctx: &AppContext) -> Result<(), Error> {
    let root = &ctx.storage.cache_dir();
    let Ok(mut rd) = tokio::fs::read_dir(&root).await else {
        return Ok(());
    };
    while let Ok(Some(entry)) = rd.next_entry().await {
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
    Ok(())
}
