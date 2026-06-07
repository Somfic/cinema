//! Trailer proxy: resolve a YouTube video id with `yt-dlp`, download a
//! browser-playable muxed mp4 to a disk cache, and hand back the cached path.
//! The HTTP layer (`raw::serve_trailer`) then range-serves it like any other
//! local file, so trailers play in a native `<video>` element with no YouTube
//! iframe or third-party requests on the client.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::app::{Error, Result, Storage};

/// Display metadata for a cached trailer.
#[derive(Clone, Copy, Serialize, Deserialize)]
pub struct TrailerMeta {
    /// Aspect ratio of the actual picture content (black bars excluded), so the
    /// frontend can size the card to it and let `object-fit: cover` clip the
    /// letterbox/pillarbox bars without re-encoding.
    pub aspect: f64,
}

/// Hard cap on how long a single yt-dlp download may run.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(90);

/// Per-key locks so concurrent requests for the same trailer download it once.
static INFLIGHT: OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> = OnceLock::new();

fn inflight() -> &'static Mutex<HashMap<String, Arc<Mutex<()>>>> {
    INFLIGHT.get_or_init(|| Mutex::new(HashMap::new()))
}

/// YouTube ids are `[A-Za-z0-9_-]`, ~11 chars. We validate strictly because the
/// key is interpolated into a command argument and a URL — this is the
/// injection / SSRF guard. The URL is always built by us, never taken raw.
fn is_valid_key(key: &str) -> bool {
    (6..=15).contains(&key.len())
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Ensure the trailer for `key` is cached on disk and return its path.
/// Downloads via `yt-dlp` on first request; subsequent requests hit the cache.
pub async fn ensure_cached(storage: &Storage, key: &str) -> Result<PathBuf> {
    if !is_valid_key(key) {
        return Err(Error::NotFound("invalid trailer key".into()));
    }

    let dir = storage.join("cache/trailers");
    let final_path = dir.join(format!("{key}.mp4"));
    if final_path.exists() {
        return Ok(final_path);
    }

    // Acquire (or create) the per-key lock so we don't spawn N yt-dlp processes
    // for the same trailer when several clients ask at once.
    let lock = {
        let mut map = inflight().lock().await;
        map.entry(key.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    };
    let _guard = lock.lock().await;

    // Another request may have finished the download while we waited.
    if final_path.exists() {
        return Ok(final_path);
    }

    tokio::fs::create_dir_all(&dir).await?;

    // yt-dlp writes the merged output to this template path. Download to a
    // temp name and rename atomically so a half-written file is never served.
    let tmp_path = dir.join(format!("{key}.part.mp4"));
    let url = format!("https://www.youtube.com/watch?v={key}");

    let result = tokio::time::timeout(
        DOWNLOAD_TIMEOUT,
        tokio::process::Command::new("yt-dlp")
            .args([
                "-f",
                // Highest avc1 (H.264, tops out at 1080p) video + m4a audio,
                // merged into a browser-safe mp4. Falls back to a muxed mp4,
                // then format 18 (360p).
                "bestvideo[ext=mp4][vcodec^=avc1][height<=1080]+bestaudio[ext=m4a]/best[ext=mp4]/18",
                "--merge-output-format",
                "mp4",
                "--no-playlist",
                "--no-progress",
                "--quiet",
                "-o",
            ])
            .arg(&tmp_path)
            .arg(&url)
            .kill_on_drop(true)
            .output(),
    )
    .await;

    let cleanup = || {
        let tmp = tmp_path.clone();
        tokio::spawn(async move {
            let _ = tokio::fs::remove_file(&tmp).await;
        });
    };

    match result {
        Ok(Ok(output)) if output.status.success() => {
            tokio::fs::rename(&tmp_path, &final_path).await?;
            Ok(final_path)
        }
        Ok(Ok(output)) => {
            cleanup();
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(Error::Generic(format!(
                "yt-dlp failed for trailer {key}: {}",
                stderr.trim()
            )))
        }
        Ok(Err(e)) => {
            cleanup();
            Err(Error::Generic(format!("failed to spawn yt-dlp: {e}")))
        }
        Err(_) => {
            cleanup();
            Err(Error::Generic(format!(
                "yt-dlp timed out after {}s for trailer {key}",
                DOWNLOAD_TIMEOUT.as_secs()
            )))
        }
    }
}

/// Return the trailer's display metadata, computing (and caching) it on first
/// request. Kept separate from `ensure_cached` so serving the video bytes is
/// never blocked on the cropdetect pass.
pub async fn ensure_meta(storage: &Storage, key: &str) -> Result<TrailerMeta> {
    if !is_valid_key(key) {
        return Err(Error::NotFound("invalid trailer key".into()));
    }
    let dir = storage.join("cache/trailers");
    let meta_path = dir.join(format!("{key}.json"));

    if let Ok(bytes) = tokio::fs::read(&meta_path).await
        && let Ok(meta) = serde_json::from_slice::<TrailerMeta>(&bytes)
    {
        return Ok(meta);
    }

    let mp4 = ensure_cached(storage, key).await?;
    // Fall back to 16:9 if detection fails, so a card always gets a sane ratio.
    let aspect = detect_content_aspect(&mp4).await.unwrap_or(16.0 / 9.0);
    let meta = TrailerMeta { aspect };
    if let Ok(bytes) = serde_json::to_vec(&meta) {
        let _ = tokio::fs::write(&meta_path, bytes).await;
    }
    Ok(meta)
}

/// Detect the aspect ratio of the picture content using ffmpeg's `cropdetect`.
/// We sample a few hundred frames and keep the *largest* detected crop (max
/// height, then width): real letterbox/pillarbox bars never contain content, so
/// the tallest crop across the sample is the true content box — this is robust
/// against dark scenes that would otherwise over-crop.
async fn detect_content_aspect(path: &Path) -> Option<f64> {
    let output = tokio::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-nostats", "-ss", "3", "-i"])
        .arg(path)
        .args([
            "-vf",
            "cropdetect=24:2:0",
            "-frames:v",
            "300",
            "-an",
            "-f",
            "null",
            "-",
        ])
        .kill_on_drop(true)
        .output()
        .await
        .ok()?;

    let stderr = String::from_utf8_lossy(&output.stderr);
    let (w, h) = parse_largest_crop(&stderr)?;
    (w > 0 && h > 0).then(|| w as f64 / h as f64)
}

/// Parse `crop=W:H:X:Y` markers from cropdetect's stderr and return the (W, H)
/// of the crop with the greatest height (tie-break: greatest width).
fn parse_largest_crop(stderr: &str) -> Option<(i64, i64)> {
    let mut best: Option<(i64, i64)> = None;
    for line in stderr.lines() {
        let Some(idx) = line.rfind("crop=") else {
            continue;
        };
        let token: String = line[idx + 5..]
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == ':')
            .collect();
        let mut parts = token.split(':');
        let (Some(Ok(w)), Some(Ok(h))) = (
            parts.next().map(str::parse::<i64>),
            parts.next().map(str::parse::<i64>),
        ) else {
            continue;
        };
        if w > 0 && h > 0 && best.is_none_or(|(bw, bh)| (h, w) > (bh, bw)) {
            best = Some((w, h));
        }
    }
    best
}
