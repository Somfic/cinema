use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use tokio::sync::Mutex;

use crate::app::{CinemaError, Result, Storage};

const FORMAT: &str =
    "bestvideo[ext=mp4][vcodec^=avc1][height<=1080]+bestaudio[ext=m4a]/best[ext=mp4]/18";

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
    // Only read cookies from a browser if one is explicitly configured. There's
    // no browser in a server/container, so defaulting to it would make yt-dlp
    // error; the anonymous case is covered by the in-process PO token instead.
    if let Some(browser) = std::env::var("CINEMA_YTDLP_COOKIES_FROM_BROWSER")
        .ok()
        .filter(|b| !b.is_empty())
    {
        cmd.arg("--cookies-from-browser").arg(browser);
    }
}

/// Point yt-dlp at an HTTP bgutil PO-token provider when one is configured.
/// Normally unset: the Docker image bundles the provider in *script* mode, so
/// yt-dlp mints proof-of-origin tokens in-process (via deno) with no server.
/// Set `CINEMA_YTDLP_POT_BASE_URL` only to use an external provider instead.
fn apply_pot(cmd: &mut tokio::process::Command) {
    if let Ok(url) = std::env::var("CINEMA_YTDLP_POT_BASE_URL")
        && !url.is_empty()
    {
        cmd.arg("--extractor-args")
            .arg(format!("youtubepot-bgutilhttp:base_url={url}"));
    }
}

/// Base URL of a self-hosted [trailers-api](https://github.com/Theryston/trailers-api)
/// used as the fallback source when YouTube fails. Unset disables the fallback.
fn trailers_api_url() -> Option<String> {
    std::env::var("CINEMA_TRAILERS_API_URL")
        .ok()
        .filter(|u| !u.is_empty())
}

/// Display metadata for a cached trailer.
#[draad::ty]
#[derive(Copy)]
pub struct TrailerMeta {
    pub aspect: f64,
}

const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(90);
/// trailers-api scrapes streaming sites on demand (concurrency 1), so its jobs
/// are slower than a yt-dlp download — give the poll loop a generous ceiling.
const TRAILERS_API_TIMEOUT: Duration = Duration::from_secs(180);

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

/// Download the trailer to the cache if it isn't there already and return its
/// path. yt-dlp does the fetching itself — this matters because YouTube's stream
/// URLs are bound to the player client yt-dlp used to obtain them, so handing
/// those raw URLs to ffmpeg gets a 403. Letting yt-dlp download sidesteps that.
///
/// YouTube is the primary source; when it fails and a `trailers-api` instance is
/// configured we fall back to it (pulls high-res trailers straight from Apple TV
/// / Netflix / Prime CDNs), matched by `title` + `year`. The cache is keyed by
/// the YouTube `key` regardless of which source won.
pub async fn ensure_cached(
    storage: &Storage,
    key: &str,
    title: Option<&str>,
    year: Option<&str>,
) -> Result<PathBuf> {
    if !is_valid_key(key) {
        return Err(CinemaError::NotFound("invalid trailer key".into()));
    }

    let dir = cache_dir(storage);
    let final_path = dir.join(format!("{key}.mp4"));
    if final_path.exists() {
        return Ok(final_path);
    }

    // De-dupe concurrent downloads of the same trailer so two viewers don't both
    // shell out to yt-dlp for the same key.
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

    let youtube_url = format!("https://www.youtube.com/watch?v={key}");
    let result =
        match download_trailer(storage, &youtube_url, &tmp_path, &format!("trailer {key}")).await {
            Ok(()) => Ok(()),
            Err(yt_err) => match (trailers_api_url(), title) {
                (Some(base), Some(title)) => {
                    tracing::warn!(
                        "youtube trailer {key} failed ({yt_err}); trying trailers-api for {title:?}"
                    );
                    trailers_api_download(&base, title, year, &tmp_path)
                    .await
                    .map_err(|api_err| {
                        CinemaError::Generic(format!(
                            "youtube failed ({yt_err}); trailers-api fallback failed ({api_err})"
                        ))
                    })
                }
                _ => Err(yt_err),
            },
        };

    match result {
        Ok(()) => {
            tokio::fs::rename(&tmp_path, &final_path).await?;
            Ok(final_path)
        }
        Err(e) => {
            let _ = tokio::fs::remove_file(&tmp_path).await;
            Err(e)
        }
    }
}

/// Run yt-dlp to download a YouTube video into `dest` as a faststart mp4.
async fn download_trailer(
    storage: &Storage,
    page_url: &str,
    dest: &Path,
    label: &str,
) -> Result<()> {
    let mut cmd = tokio::process::Command::new("yt-dlp");
    cmd.args([
        "-f",
        FORMAT,
        "--merge-output-format",
        "mp4",
        "--no-playlist",
        "--no-progress",
        "--quiet",
        // Move the moov atom to the front so the browser can start playing (and
        // seeking) as soon as the file is served, rather than after fetching it all.
        "--postprocessor-args",
        "ffmpeg:-movflags +faststart",
    ]);
    apply_cookies(&mut cmd, storage);
    apply_pot(&mut cmd);
    cmd.arg("-o").arg(dest).arg(page_url).kill_on_drop(true);

    let output = tokio::time::timeout(DOWNLOAD_TIMEOUT, cmd.output())
        .await
        .map_err(|_| {
            CinemaError::Generic(format!(
                "yt-dlp timed out after {}s for {label}",
                DOWNLOAD_TIMEOUT.as_secs()
            ))
        })?
        .map_err(|e| CinemaError::Generic(format!("failed to spawn yt-dlp: {e}")))?;

    if !output.status.success() {
        return Err(CinemaError::Generic(format!(
            "yt-dlp failed for {label}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(())
}

/// Fetch a trailer via a self-hosted trailers-api: submit a job for the title,
/// poll until it's done, then download the resulting file into `dest`.
async fn trailers_api_download(
    base: &str,
    title: &str,
    year: Option<&str>,
    dest: &Path,
) -> Result<()> {
    let base = base.trim_end_matches('/');
    let client = reqwest::Client::new();

    // 1) Submit the job.
    let mut body = serde_json::json!({ "name": title });
    if let Some(y) = year {
        body["year"] = serde_json::Value::from(y);
    }
    let submit: ProcessSubmit = client
        .post(format!("{base}/process"))
        .json(&body)
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| CinemaError::Generic(format!("trailers-api submit failed: {e}")))?
        .json()
        .await
        .map_err(|e| CinemaError::Generic(format!("trailers-api submit parse failed: {e}")))?;

    // 2) Poll until the job finishes.
    let deadline = tokio::time::Instant::now() + TRAILERS_API_TIMEOUT;
    let file_url = loop {
        if tokio::time::Instant::now() >= deadline {
            return Err(CinemaError::Generic(format!(
                "trailers-api timed out for {title:?}"
            )));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;

        let status: ProcessStatus = client
            .get(format!("{base}/process/{}", submit.process_id))
            .timeout(Duration::from_secs(15))
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|e| CinemaError::Generic(format!("trailers-api poll failed: {e}")))?
            .json()
            .await
            .map_err(|e| CinemaError::Generic(format!("trailers-api poll parse failed: {e}")))?;

        match status.status.as_str() {
            "done" => match status.trailers.into_iter().next() {
                Some(t) => break t.url,
                None => {
                    return Err(CinemaError::Generic(format!(
                        "trailers-api returned no trailer for {title:?}"
                    )));
                }
            },
            "error" | "no_trailers" | "cancelled" => {
                return Err(CinemaError::Generic(format!(
                    "trailers-api {} for {title:?}",
                    status.status
                )));
            }
            // pending / processing / finding_trailer_page / trying_to_download / …
            _ => continue,
        }
    };

    // 3) Download the finished file.
    let bytes = client
        .get(&file_url)
        .timeout(Duration::from_secs(120))
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| CinemaError::Generic(format!("trailers-api download failed: {e}")))?
        .bytes()
        .await
        .map_err(|e| CinemaError::Generic(format!("trailers-api read failed: {e}")))?;
    tokio::fs::write(dest, &bytes).await?;
    Ok(())
}

#[derive(serde::Deserialize)]
struct ProcessSubmit {
    #[serde(alias = "processId", alias = "process_id", alias = "id")]
    process_id: String,
}

#[derive(serde::Deserialize)]
struct ProcessStatus {
    status: String,
    #[serde(default)]
    trailers: Vec<ProcessTrailer>,
}

#[derive(serde::Deserialize)]
struct ProcessTrailer {
    url: String,
}

pub fn cached_path(storage: &Storage, key: &str) -> Option<PathBuf> {
    if !is_valid_key(key) {
        return None;
    }
    let path = cache_dir(storage).join(format!("{key}.mp4"));
    path.exists().then_some(path)
}

pub async fn ensure_meta(storage: &Storage, key: &str) -> Result<TrailerMeta> {
    if !is_valid_key(key) {
        return Err(CinemaError::NotFound("invalid trailer key".into()));
    }
    let dir = cache_dir(storage);
    let meta_path = dir.join(format!("{key}.json"));

    if let Ok(bytes) = tokio::fs::read(&meta_path).await
        && let Ok(meta) = serde_json::from_slice::<TrailerMeta>(&bytes)
    {
        return Ok(meta);
    }

    // Aspect is detected from the downloaded file. If the trailer isn't cached yet
    // (still downloading, or never played) return the default without persisting,
    // so a later call re-detects once the file exists.
    let Some(path) = cached_path(storage, key) else {
        return Ok(TrailerMeta { aspect: 16.0 / 9.0 });
    };
    let aspect = detect_content_aspect(&path.to_string_lossy())
        .await
        .unwrap_or(16.0 / 9.0);
    let meta = TrailerMeta { aspect };
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
