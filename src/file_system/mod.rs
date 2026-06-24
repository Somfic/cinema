use std::path::Path;

use crate::AppContext;
use crate::app::Error;

mod cache;
pub(crate) mod stats; // This doesn't have to be public, but draad requires the path to resolve

pub use cache::{CacheEntry, clear_app_cache, delete_cache_orphan, list_cache_items};
pub use stats::{DiskStats, get_cache_disk};

type SizeInBytes = u64;

/// Recursively sum the size of all regular files under `path`.
/// Symlinks are not followed. Missing or unreadable entries contribute 0.
async fn dir_size(path: &Path) -> SizeInBytes {
    let mut total: u64 = 0;
    let mut stack: Vec<std::path::PathBuf> = vec![path.to_path_buf()];

    while let Some(dir) = stack.pop() {
        let mut entries = match tokio::fs::read_dir(&dir).await {
            Ok(e) => e,
            Err(_) => continue,
        };

        while let Ok(Some(entry)) = entries.next_entry().await {
            let Ok(ft) = entry.file_type().await else {
                continue;
            };
            if ft.is_symlink() {
                continue;
            }
            if ft.is_dir() {
                stack.push(entry.path());
            } else if ft.is_file()
                && let Ok(meta) = entry.metadata().await
            {
                total = total.saturating_add(meta.len());
            }
        }
    }

    total
}

/// Filesystem stats for the volume containing `path`.
/// Returns (total_bytes, free_bytes available to non-root).
fn fs_stats(path: &Path) -> Result<(SizeInBytes, SizeInBytes), Error> {
    #[cfg(unix)]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let c_path = CString::new(path.as_os_str().as_bytes())
            .map_err(|e| Error::Generic(format!("invalid path for statvfs: {e}")))?;

        let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
        let rc = unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) };
        if rc != 0 {
            return Err(Error::IoError(std::io::Error::last_os_error()));
        }

        let frsize = stat.f_frsize;

        let total = frsize.saturating_mul(stat.f_blocks);
        let free = frsize.saturating_mul(stat.f_bavail);
        Ok((total, free))
    }
    #[cfg(not(unix))]
    {
        Err(Error::Generic(
            "fs_stats not supported on this platform".into(),
        ))
    }
}

fn torrents_root(ctx: &AppContext) -> std::path::PathBuf {
    ctx.storage.join("torrents")
}

fn hls_root(ctx: &AppContext) -> std::path::PathBuf {
    ctx.storage.join("hls")
}
