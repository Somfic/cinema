use crate::{
    app::{AppContext, Error},
    file_system,
};

#[draad::ty]
#[serde(rename_all = "lowercase")]
pub enum EntryKind {
    Download,
    // TODO: HLS is not implemented. The idea is that users can "pretranscode" files (like
    // queueing a download), which will get this type
    Hls,
    Orphan,
}

#[draad::ty]
pub struct CacheEntry {
    /// "download" for tracked downloads, "orphan" for stray torrent dirs.
    kind: EntryKind,
    info_hash: String,
    /// the corresponding download entry. None for orphans
    download: Option<crate::downloads::types::Download>,
    /// Actual on-disk size of the torrent directory.
    disk_bytes: u64,
}

pub async fn list_cache_items(ctx: &AppContext) -> Result<Vec<CacheEntry>, Error> {
    let downloads = crate::downloads::types::Download::find_all(&ctx.db).await?;

    let torrents = file_system::torrents_root(ctx);
    let mut seen_hashes: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut entries: Vec<CacheEntry> = Vec::with_capacity(downloads.len());

    for download in downloads {
        let dir = torrents.join(&download.info_hash);
        let disk_bytes = file_system::dir_size(&dir).await;
        seen_hashes.insert(download.info_hash.to_lowercase());

        entries.push(CacheEntry {
            kind: EntryKind::Download,
            info_hash: download.info_hash.clone(),
            download: Some(download),
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
