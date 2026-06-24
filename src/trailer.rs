use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use tokio::process::{Child, ChildStdout};
use tokio::sync::Mutex;

use crate::app::{Error, Result, Storage};

const FORMAT: &str =
    "bestvideo[ext=mp4][vcodec^=avc1][height<=1080]+bestaudio[ext=m4a]/best[ext=mp4]/18";

const DETECT_FORMAT: &str =
    "worstvideo[ext=mp4][height>=240]/worstvideo[ext=mp4]/worst[ext=mp4]/worst";

fn cache_dir(storage: &Storage) -> PathBuf {
    storage.join("cache/trailers")
}

/// Path of the uploaded cookies file inside the data dir. Always returned
/// (even when missing) so callers can write to it.
pub fn cookies_storage_path(storage: &Storage) -> PathBuf {
    storage.join("youtube-cookies.txt")
}

/// The env var takes precedence over the data-dir file so an operator can
/// force a specific path — return it so the UI can surface that.
pub fn cookies_env_override() -> Option<String> {
    std::env::var("CINEMA_YTDLP_COOKIES")
        .ok()
        .filter(|p| std::path::Path::new(p).exists())
}

fn cookies_file(storage: &Storage) -> Option<PathBuf> {
    if let Ok(p) = std::env::var("CINEMA_YTDLP_COOKIES") {
        let path = PathBuf::from(p);
        if path.exists() {
            return Some(path);
        }
    }
    let path = cookies_storage_path(storage);
    path.exists().then_some(path)
}

fn apply_cookies(cmd: &mut tokio::process::Command, storage: &Storage) {
    if let Some(path) = cookies_file(storage) {
        cmd.arg("--cookies").arg(path);
        return;
    }
    let browser = std::env::var("CINEMA_YTDLP_COOKIES_FROM_BROWSER")
        .ok()
        .filter(|b| !b.is_empty())
        .unwrap_or_else(|| "chrome".to_string());
    cmd.arg("--cookies-from-browser").arg(browser);
}

/// Display metadata for a cached trailer.
#[draad::ty]
#[derive(Copy)]
pub struct TrailerMeta {
    pub aspect: f64,
}

const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(90);

static INFLIGHT: OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> = OnceLock::new();

fn inflight() -> &'static Mutex<HashMap<String, Arc<Mutex<()>>>> {
    INFLIGHT.get_or_init(|| Mutex::new(HashMap::new()))
}

fn is_valid_key(key: &str) -> bool {
    (6..=15).contains(&key.len())
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

pub async fn ensure_cached(storage: &Storage, key: &str) -> Result<PathBuf> {
    if !is_valid_key(key) {
        return Err(Error::NotFound("invalid trailer key".into()));
    }

    let dir = cache_dir(storage);
    let final_path = dir.join(format!("{key}.mp4"));
    if final_path.exists() {
        return Ok(final_path);
    }

    let lock = {
        let mut map = inflight().lock().await;
        map.entry(key.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    };
    let _guard = lock.lock().await;

    if final_path.exists() {
        return Ok(final_path);
    }

    tokio::fs::create_dir_all(&dir).await?;

    let tmp_path = dir.join(format!("{key}.part.mp4"));
    let url = format!("https://www.youtube.com/watch?v={key}");

    let mut cmd = tokio::process::Command::new("yt-dlp");
    cmd.args([
        "-f",
        FORMAT,
        "--merge-output-format",
        "mp4",
        "--no-playlist",
        "--no-progress",
        "--quiet",
    ]);
    apply_cookies(&mut cmd, storage);
    cmd.arg("-o").arg(&tmp_path).arg(&url).kill_on_drop(true);

    let result = tokio::time::timeout(DOWNLOAD_TIMEOUT, cmd.output()).await;

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

pub fn cached_path(storage: &Storage, key: &str) -> Option<PathBuf> {
    if !is_valid_key(key) {
        return None;
    }
    let path = cache_dir(storage).join(format!("{key}.mp4"));
    path.exists().then_some(path)
}

pub struct TrailerStream {
    pub child: Child,
    pub stdout: ChildStdout,
    pub tmp_path: PathBuf,
    pub final_path: PathBuf,
}

pub async fn start_stream(storage: &Storage, key: &str) -> Result<TrailerStream> {
    if !is_valid_key(key) {
        return Err(Error::NotFound("invalid trailer key".into()));
    }
    let dir = cache_dir(storage);
    tokio::fs::create_dir_all(&dir).await?;

    let urls = resolve_urls(storage, key, FORMAT).await?;

    let mut cmd = tokio::process::Command::new("ffmpeg");
    cmd.args(["-hide_banner", "-loglevel", "error"]);
    for url in &urls {
        cmd.arg("-i").arg(url);
    }
    if urls.len() >= 2 {
        cmd.args(["-map", "0:v:0", "-map", "1:a:0"]);
    }
    cmd.args([
        "-c",
        "copy",
        "-movflags",
        "frag_keyframe+empty_moov+default_base_moof",
        "-f",
        "mp4",
        "pipe:1",
    ]);

    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| Error::Generic(format!("failed to spawn ffmpeg: {e}")))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| Error::Generic("ffmpeg produced no stdout".into()))?;

    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);

    Ok(TrailerStream {
        child,
        stdout,
        tmp_path: dir.join(format!("{key}.stream.{seq}.part.mp4")),
        final_path: dir.join(format!("{key}.mp4")),
    })
}

async fn resolve_urls(storage: &Storage, key: &str, format: &str) -> Result<Vec<String>> {
    let url = format!("https://www.youtube.com/watch?v={key}");
    let mut cmd = tokio::process::Command::new("yt-dlp");
    cmd.args(["-f", format, "--no-playlist", "-g"]);
    apply_cookies(&mut cmd, storage);
    cmd.arg(&url).kill_on_drop(true);

    let output = tokio::time::timeout(DOWNLOAD_TIMEOUT, cmd.output())
        .await
        .map_err(|_| Error::Generic("yt-dlp -g timed out".into()))?
        .map_err(|e| Error::Generic(format!("failed to spawn yt-dlp: {e}")))?;

    if !output.status.success() {
        return Err(Error::Generic(format!(
            "yt-dlp -g failed for trailer {key}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }

    let urls: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    if urls.is_empty() {
        return Err(Error::Generic(format!(
            "yt-dlp returned no media URL for trailer {key}"
        )));
    }
    Ok(urls)
}

pub async fn ensure_meta(storage: &Storage, key: &str) -> Result<TrailerMeta> {
    if !is_valid_key(key) {
        return Err(Error::NotFound("invalid trailer key".into()));
    }
    let dir = cache_dir(storage);
    let meta_path = dir.join(format!("{key}.json"));

    if let Ok(bytes) = tokio::fs::read(&meta_path).await
        && let Ok(meta) = serde_json::from_slice::<TrailerMeta>(&bytes)
    {
        return Ok(meta);
    }

    let detected = match cached_path(storage, key) {
        Some(path) => detect_content_aspect(&path.to_string_lossy()).await,
        None => match resolve_urls(storage, key, DETECT_FORMAT).await {
            Ok(urls) => match urls.first() {
                Some(url) => detect_content_aspect(url).await,
                None => None,
            },
            Err(_) => None,
        },
    };
    let meta = TrailerMeta {
        aspect: detected.unwrap_or(16.0 / 9.0),
    };
    if let Ok(bytes) = serde_json::to_vec(&meta) {
        let _ = tokio::fs::write(&meta_path, bytes).await;
    }
    Ok(meta)
}

async fn detect_content_aspect(input: &str) -> Option<f64> {
    let output = tokio::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-nostats", "-ss", "3", "-i"])
        .arg(input)
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
