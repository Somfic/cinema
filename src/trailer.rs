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

// IMDb serves progressive (muxed) mp4s off Amazon's CDN rather than the
// separate video/audio streams YouTube exposes, so the format filter is simpler.
const IMDB_FORMAT: &str = "best[ext=mp4][height<=1080]/best[height<=1080]/best";

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

/// Point yt-dlp at a bgutil PO-token provider when one is configured. YouTube
/// increasingly requires a proof-of-origin token from datacenter IPs; the
/// provider (a sidecar server) mints them and the bundled yt-dlp plugin fetches
/// them automatically. No-op when `CINEMA_YTDLP_POT_BASE_URL` is unset.
fn apply_pot(cmd: &mut tokio::process::Command) {
    if let Ok(url) = std::env::var("CINEMA_YTDLP_POT_BASE_URL")
        && !url.is_empty()
    {
        cmd.arg("--extractor-args")
            .arg(format!("youtubepot-bgutilhttp:base_url={url}"));
    }
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

/// IMDb title ids are `tt` followed by 7+ digits (e.g. `tt0468569`).
fn is_valid_imdb_id(id: &str) -> bool {
    (9..=12).contains(&id.len())
        && id.starts_with("tt")
        && id.as_bytes()[2..].iter().all(u8::is_ascii_digit)
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
    apply_pot(&mut cmd);
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

pub async fn start_stream(
    storage: &Storage,
    key: &str,
    imdb_id: Option<&str>,
) -> Result<TrailerStream> {
    if !is_valid_key(key) {
        return Err(Error::NotFound("invalid trailer key".into()));
    }
    let dir = cache_dir(storage);
    tokio::fs::create_dir_all(&dir).await?;

    // YouTube is the primary source but frequently blocks anonymous/datacenter
    // requests. When it fails and we know the title's IMDb id, fall back to the
    // trailer IMDb hosts on Amazon's CDN (no bot-blocking). The cache stays keyed
    // by the YouTube `key`, so later requests hit the fast path regardless.
    let urls = match resolve_urls(storage, key, FORMAT).await {
        Ok(urls) => urls,
        Err(yt_err) => match imdb_id.filter(|id| is_valid_imdb_id(id)) {
            Some(id) => {
                tracing::warn!("youtube trailer {key} failed ({yt_err}); trying imdb {id}");
                resolve_imdb_urls(storage, id).await.map_err(|imdb_err| {
                    Error::Generic(format!(
                        "youtube failed ({yt_err}); imdb fallback failed ({imdb_err})"
                    ))
                })?
            }
            None => return Err(yt_err),
        },
    };

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
    resolve_media_urls(storage, &url, format, true, &format!("trailer {key}")).await
}

/// Resolve a source page to direct media URLs via `yt-dlp -g`. Cookies and the
/// PO-token provider only apply to YouTube; IMDb is public and passing
/// `--cookies-from-browser` in a headless environment would make yt-dlp error.
async fn resolve_media_urls(
    storage: &Storage,
    page_url: &str,
    format: &str,
    youtube: bool,
    label: &str,
) -> Result<Vec<String>> {
    let mut cmd = tokio::process::Command::new("yt-dlp");
    cmd.args(["-f", format, "--no-playlist", "-g"]);
    if youtube {
        apply_cookies(&mut cmd, storage);
        apply_pot(&mut cmd);
    }
    cmd.arg(page_url).kill_on_drop(true);

    let output = tokio::time::timeout(DOWNLOAD_TIMEOUT, cmd.output())
        .await
        .map_err(|_| Error::Generic(format!("yt-dlp -g timed out for {label}")))?
        .map_err(|e| Error::Generic(format!("failed to spawn yt-dlp: {e}")))?;

    if !output.status.success() {
        return Err(Error::Generic(format!(
            "yt-dlp -g failed for {label}: {}",
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
            "yt-dlp returned no media URL for {label}"
        )));
    }
    Ok(urls)
}

/// Resolve a title's IMDb trailer to direct media URLs: scrape the title page for
/// its primary video id (`vi…`), then hand that IMDb video page to yt-dlp.
async fn resolve_imdb_urls(storage: &Storage, imdb_id: &str) -> Result<Vec<String>> {
    let vi = imdb_trailer_video_id(imdb_id).await?;
    let url = format!("https://www.imdb.com/video/{vi}/");
    resolve_media_urls(storage, &url, IMDB_FORMAT, false, &format!("imdb {imdb_id} {vi}")).await
}

/// Find a title's trailer video id (`vi…`) via IMDb's suggestion API. This lives
/// on a CDN host (`*.media-imdb.com`) that returns plain JSON, unlike the main
/// `imdb.com` site which bot-walls datacenter requests. Prefers a video whose
/// label mentions "trailer", else the first (the primary/hero video).
async fn imdb_trailer_video_id(imdb_id: &str) -> Result<String> {
    let url =
        format!("https://v3.sg.media-imdb.com/suggestion/t/{imdb_id}.json?includeVideos=1");
    let client = reqwest::Client::builder()
        .user_agent(
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 \
             (KHTML, like Gecko) Chrome/125.0 Safari/537.36",
        )
        .build()
        .map_err(|e| Error::Generic(format!("failed to build http client: {e}")))?;

    let resp: ImdbSuggestion = tokio::time::timeout(Duration::from_secs(15), async {
        client.get(&url).send().await?.error_for_status()?.json().await
    })
    .await
    .map_err(|_| Error::Generic(format!("imdb suggestion timed out for {imdb_id}")))?
    .map_err(|e| Error::Generic(format!("imdb suggestion failed for {imdb_id}: {e}")))?;

    let item = resp
        .d
        .iter()
        .find(|i| i.id == imdb_id)
        .ok_or_else(|| Error::Generic(format!("imdb suggestion had no entry for {imdb_id}")))?;
    let pick = item
        .v
        .iter()
        .find(|v| v.l.to_ascii_lowercase().contains("trailer"))
        .or_else(|| item.v.first())
        .ok_or_else(|| Error::Generic(format!("imdb has no videos for {imdb_id}")))?;

    // Guard against a malformed id flowing into a yt-dlp URL.
    if !(pick.id.starts_with("vi") && pick.id.len() > 2 && pick.id[2..].bytes().all(|b| b.is_ascii_digit()))
    {
        return Err(Error::Generic(format!(
            "imdb returned malformed video id {}",
            pick.id
        )));
    }
    Ok(pick.id.clone())
}

#[derive(serde::Deserialize)]
struct ImdbSuggestion {
    #[serde(default)]
    d: Vec<ImdbSuggestItem>,
}

#[derive(serde::Deserialize)]
struct ImdbSuggestItem {
    id: String,
    #[serde(default)]
    v: Vec<ImdbSuggestVideo>,
}

#[derive(serde::Deserialize)]
struct ImdbSuggestVideo {
    id: String,
    #[serde(default)]
    l: String,
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
