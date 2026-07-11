use crate::{
    app::{AppContext, Error},
    file_system,
};

#[draad::ty]
pub struct DiskStats {
    /// Total filesystem size containing data_dir.
    total_bytes: u64,
    /// Free bytes on that filesystem (available to non-root).
    free_bytes: u64,
    /// total_bytes - free_bytes.
    used_bytes: u64,
    /// Size of the entire cinema data_dir.
    cinema_bytes: u64,
    /// Size of data_dir/fs/torrents (all per-info-hash dirs).
    torrents_bytes: u64,
    /// Size of data_dir/fs/hls (active live transcoding sessions only).
    hls_bytes: u64,
    /// Size of data_dir/fs/pretranscoded (background pretranscoding jobs).
    pretranscoding_bytes: u64,
    /// Size of data_dir/fs/cache.
    cache_bytes: u64,
    /// Subtotal of torrent dirs belonging to category=="movies".
    movies_bytes: u64,
    /// Subtotal of torrent dirs belonging to category=="tv".
    tv_bytes: u64,
    /// Subtotal of orphan torrent dirs (no DB row).
    orphan_bytes: u64,
}

pub async fn get_cache_disk(ctx: &AppContext) -> Result<DiskStats, Error> {
    let (total_bytes, free_bytes) = file_system::fs_stats(&ctx.config.data_dir)?;
    let used_bytes = total_bytes.saturating_sub(free_bytes);

    let cinema_bytes = file_system::dir_size(&ctx.config.data_dir).await;
    let hls_bytes = file_system::dir_size(&ctx.storage.hls_dir()).await;
    let pretranscoding_bytes = file_system::dir_size(&ctx.storage.pretranscoded_dir()).await;
    let cache_bytes = file_system::dir_size(&ctx.storage.cache_dir()).await;

    // Per-category torrent breakdown matches list_cache_items so the chart and list agree.
    let downloads = crate::downloads::types::Download::find_all(&ctx.db).await?;

    let torrents = ctx.storage.torrents_dir();
    let mut seen_hashes: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut movies_bytes: u64 = 0;
    let mut tv_bytes: u64 = 0;
    let mut tracked_bytes: u64 = 0;

    for download in downloads {
        let size = file_system::dir_size(&download.output_path(&ctx.storage)).await;
        seen_hashes.insert(download.info_hash.to_lowercase());
        tracked_bytes = tracked_bytes.saturating_add(size);
        match download
            .meta
            .as_ref()
            .and_then(|m| m.media_item.as_ref())
            .map(|m| m.media_type)
        {
            Some(crate::tmdb::MediaType::Movie) => movies_bytes = movies_bytes.saturating_add(size),
            Some(crate::tmdb::MediaType::Tv) => tv_bytes = tv_bytes.saturating_add(size),
            None => {}
        }
    }

    let mut orphan_bytes: u64 = 0;
    if let Ok(mut rd) = tokio::fs::read_dir(&torrents).await {
        while let Ok(Some(entry)) = rd.next_entry().await {
            let Ok(ft) = entry.file_type().await else {
                continue;
            };
            if !ft.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_lowercase();
            if seen_hashes.contains(&name) {
                continue;
            }
            orphan_bytes = orphan_bytes.saturating_add(file_system::dir_size(&entry.path()).await);
        }
    }

    let torrents_bytes = tracked_bytes.saturating_add(orphan_bytes);

    Ok(DiskStats {
        total_bytes,
        free_bytes,
        used_bytes,
        cinema_bytes,
        torrents_bytes,
        hls_bytes,
        pretranscoding_bytes,
        cache_bytes,
        movies_bytes,
        tv_bytes,
        orphan_bytes,
    })
}
