use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use tokio::sync::Mutex;
use tokio::sync::watch;

static SESSIONS: OnceLock<Mutex<HashMap<String, HlsSession>>> = OnceLock::new();

use std::sync::OnceLock;

pub mod ffmpeg;
mod pipeline;

struct HlsSession {
    dir: PathBuf,
    child: tokio::process::Child,
    last_access: Instant,
    /// Receives the ffmpeg error message when the process exits with failure.
    /// `None` means still running, `Some(msg)` means exited with that error.
    exit_error: watch::Receiver<Option<String>>,
    abort_handles: Vec<tokio::task::AbortHandle>,
}

impl Drop for HlsSession {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
        for handle in &self.abort_handles {
            handle.abort();
        }
        let dir = std::mem::take(&mut self.dir);
        tokio::spawn(async move {
            let _ = tokio::fs::remove_dir_all(&dir).await;
        });
    }
}

fn sessions() -> &'static Mutex<HashMap<String, HlsSession>> {
    SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Generate a random 16-char hex session ID.
fn new_session_id() -> String {
    use std::time::SystemTime;
    let t = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    // Mix time with a random-ish value from stack address
    let stack_val = &t as *const _ as usize;
    format!("{:08x}{:08x}", t as u32, stack_val as u32)
}

/// Browser-safe video codecs that can be copied directly into HLS.
const BROWSER_SAFE_VIDEO: &[&str] = &["h264", "avc", "avc1"];

pub async fn is_browser_safe(path: &std::path::Path) -> bool {
    let video_codec = probe_video_codec(path).await.unwrap_or_default();
    BROWSER_SAFE_VIDEO.iter().any(|c| video_codec.contains(c))
}

/// Probe the video codec of a file using ffprobe. `input` accepts either an
/// on-disk path or an ffmpeg-style URL/pipe descriptor.
async fn probe_video_codec(path: &std::path::Path) -> Option<String> {
    let output = tokio::process::Command::new("ffprobe")
        .args([
            "-v",
            "quiet",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=codec_name",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let codec = String::from_utf8_lossy(&output.stdout)
        .trim()
        .to_lowercase();
    if codec.is_empty() { None } else { Some(codec) }
}

pub struct HlsSessionStartInput {
    pub audio_index: usize,
    pub start_time: f64,
    pub only_audio: bool,
}

/// Start an HLS remux session. Returns (session_id, playlist_path).
/// Spawns ffmpeg reading from the given `source` (disk or engine) and writing
/// HLS segments to a temp directory. If `start_time` > 0, ffmpeg seeks to that
/// position before encoding.
pub async fn start_session(
    storage: &crate::app::Storage,
    config: &crate::Config,
    source: crate::downloads::MediaSource,
    session_input: HlsSessionStartInput,
) -> crate::app::Result<(String, String)> {
    let session_id = new_session_id();
    let dir = storage.join(format!("hls/{session_id}"));
    tokio::fs::create_dir_all(&dir).await?;

    match start_transcoding(config, source, session_input, &session_id, dir.clone()).await {
        Ok(url) => Ok((session_id, url)),
        Err(err) => {
            let _ = tokio::fs::remove_dir_all(dir).await;
            Err(err)
        }
    }
}

async fn start_transcoding(
    config: &crate::Config,
    source: crate::downloads::MediaSource,
    session_input: HlsSessionStartInput,
    session_id: &String,
    dir: PathBuf,
) -> crate::app::Result<String> {
    let playlist_path = dir.join("playlist.m3u8");
    let segment_pattern = dir.join("seg%05d.ts");

    let copy_video = session_input.only_audio || is_browser_safe(source.probe_path()).await;
    let input_display = source.probe_path().display().to_string();

    let mut child = ffmpeg::transcode(
        config,
        &source,
        copy_video,
        session_input.start_time,
        session_input.audio_index,
        &playlist_path,
        &segment_pattern,
    )
    .await
    .spawn()
    .map_err(|e| crate::app::Error::Generic(format!("Failed to start ffmpeg HLS: {e}")))?;

    // For Engine sources we need to pump bytes into ffmpeg's stdin (so it
    // blocks on missing pieces rather than hitting EOF). Disk sources use
    // `-i <path>` and don't need a pump task.
    let write_task = if matches!(
        source.ffmpeg_input_spec(),
        crate::downloads::FfmpegInputSpec::Pipe
    ) {
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| crate::app::Error::Generic("Failed to open ffmpeg stdin".into()))?;
        Some(source.spawn_stdin_pump(stdin).await?)
    } else {
        None
    };

    // Capture stderr and track process exit
    let (exit_tx, exit_rx) = watch::channel(None);
    let stderr = child.stderr.take();
    let sid = session_id.clone();
    let span = tracing::Span::current();

    let error_task = tokio::spawn(tracing::Instrument::instrument(
        async move {
            use tokio::io::{AsyncBufReadExt, BufReader};

            let Some(stderr) = stderr else { return };
            let mut reader = BufReader::new(stderr);
            let mut line = String::new();
            let mut last_lines: Vec<String> = Vec::new();

            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) => break, // EOF — ffmpeg exited
                    Ok(_) => {
                        let trimmed = line.trim();
                        if !trimmed.is_empty() {
                            tracing::trace!(session = %sid, "ffmpeg: {trimmed}");
                            // Keep last 5 lines for error reporting
                            if last_lines.len() >= 5 {
                                last_lines.remove(0);
                            }
                            last_lines.push(trimmed.to_string());
                        }
                    }
                    Err(e) => {
                        tracing::warn!(session = %sid, "ffmpeg stderr read error: {e}");
                        break;
                    }
                }
            }

            // ffmpeg has exited — check if it completed successfully or failed
            let has_error = last_lines.iter().any(|l| {
                l.contains("Error")
                    || l.contains("error")
                    || l.contains("Invalid")
                    || l.contains("No such file")
            });

            if has_error {
                let error_context = last_lines.join("\n");
                tracing::warn!(session = %sid, input = %input_display, "ffmpeg failed: {error_context}");
                let _ = exit_tx.send(Some(error_context));
            } else {
                tracing::info!(session = %sid, input = %input_display, "ffmpeg finished transcoding successfully");
            }
        },
        span,
    ));

    let mut abort_handles = vec![error_task.abort_handle()];
    if let Some(ref w) = write_task {
        abort_handles.push(w.abort_handle());
    }

    sessions().lock().await.insert(
        session_id.clone(),
        HlsSession {
            dir,
            child,
            last_access: Instant::now(),
            exit_error: exit_rx,
            abort_handles,
        },
    );

    // Wait for the playlist to have at least one segment
    let result = tokio::time::timeout(config.ffmpeg_max_startup_duration, async {
        loop {
            // Check if ffmpeg already died before producing any segments
            if let Some(error) = session_error(session_id).await {
                return Err(crate::app::Error::Generic(format!(
                    "ffmpeg failed: {error}"
                )));
            }

            if let Ok(content) = tokio::fs::read_to_string(&playlist_path).await
                && content.contains("#EXTINF")
            {
                return Ok(());
            }
            tokio::time::sleep(config.ffmpeg_startup_poll_interval).await;
        }
    })
    .await;

    match result {
        Ok(Ok(())) => {} // success
        Ok(Err(e)) => {
            stop_session(session_id).await;
            return Err(e);
        } // ffmpeg error
        Err(_) => {
            stop_session(session_id).await;
            return Err(crate::app::Error::Generic(String::from(
                "ffmpeg startup timeout",
            )));
        }
    };

    let url = format!("/api/hls/{session_id}/playlist.m3u8");
    Ok(url)
}

/// Start an HLS remux session that reads a local pretranscoded MP4 (already
/// browser-safe codecs) instead of a live torrent. Because the source is a
/// completed on-disk file with the moov atom at the front (see `pretranscodings::supervisor`),
/// ffmpeg can seek instantly and copy both streams into HLS without a re-encode.
pub async fn start_session_from_local(
    storage: &crate::app::Storage,
    config: &crate::Config,
    path: impl Into<PathBuf>,
    start_time: f64,
) -> crate::app::Result<(String, String)> {
    let session_id = new_session_id();
    let dir = storage.join(format!("hls/{session_id}"));
    tokio::fs::create_dir_all(&dir).await?;

    match start_local_transcoding(config, path.into(), start_time, &session_id, dir.clone()).await {
        Ok(url) => Ok((session_id, url)),
        Err(err) => {
            let _ = tokio::fs::remove_dir_all(dir).await;
            Err(err)
        }
    }
}

async fn start_local_transcoding(
    config: &crate::Config,
    path: PathBuf,
    start_time: f64,
    session_id: &String,
    dir: PathBuf,
) -> crate::app::Result<String> {
    let playlist_path = dir.join("playlist.m3u8");
    let segment_pattern = dir.join("seg%05d.ts");

    let input_display = path.display().to_string();
    let mut child = ffmpeg::local_transcode(start_time, &path, &playlist_path, &segment_pattern)
        .spawn()
        .map_err(|e| crate::app::Error::Generic(format!("Failed to start ffmpeg HLS: {e}")))?;

    let (exit_tx, exit_rx) = watch::channel(None);
    let stderr = child.stderr.take();
    let sid = session_id.clone();
    let span = tracing::Span::current();

    let error_task = tokio::spawn(tracing::Instrument::instrument(
        async move {
            use tokio::io::{AsyncBufReadExt, BufReader};

            let Some(stderr) = stderr else { return };
            let mut reader = BufReader::new(stderr);
            let mut line = String::new();
            let mut last_lines: Vec<String> = Vec::new();

            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) => break,
                    Ok(_) => {
                        let trimmed = line.trim();
                        if !trimmed.is_empty() {
                            tracing::trace!(session = %sid, "ffmpeg: {trimmed}");
                            if last_lines.len() >= 5 {
                                last_lines.remove(0);
                            }
                            last_lines.push(trimmed.to_string());
                        }
                    }
                    Err(e) => {
                        tracing::warn!(session = %sid, "ffmpeg stderr read error: {e}");
                        break;
                    }
                }
            }

            let has_error = last_lines.iter().any(|l| {
                l.contains("Error")
                    || l.contains("error")
                    || l.contains("Invalid")
                    || l.contains("No such file")
            });
            if has_error {
                let error_context = last_lines.join("\n");
                tracing::warn!(session = %sid, input = %input_display, "ffmpeg failed: {error_context}");
                let _ = exit_tx.send(Some(error_context));
            } else {
                tracing::info!(session = %sid, input = %input_display, "ffmpeg finished remux successfully");
            }
        },
        span,
    ));

    sessions().lock().await.insert(
        session_id.clone(),
        HlsSession {
            dir,
            child,
            last_access: Instant::now(),
            exit_error: exit_rx,
            abort_handles: vec![error_task.abort_handle()],
        },
    );

    let result = tokio::time::timeout(config.ffmpeg_max_startup_duration, async {
        loop {
            if let Some(error) = session_error(session_id).await {
                return Err(crate::app::Error::Generic(format!(
                    "ffmpeg failed: {error}"
                )));
            }

            if let Ok(content) = tokio::fs::read_to_string(&playlist_path).await
                && content.contains("#EXTINF")
            {
                return Ok(());
            }
            tokio::time::sleep(config.ffmpeg_startup_poll_interval).await;
        }
    })
    .await;

    match result {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            stop_session(session_id).await;
            return Err(e);
        }
        Err(_) => {
            stop_session(session_id).await;
            return Err(crate::app::Error::Generic(String::from(
                "ffmpeg startup timeout",
            )));
        }
    }

    let url = format!("/api/hls/{session_id}/playlist.m3u8");
    Ok(url)
}

/// Touch the session's last_access timestamp.
pub async fn touch(session_id: &str) {
    if let Some(session) = sessions().lock().await.get_mut(session_id) {
        session.last_access = Instant::now();
    }
}

/// Get the directory for a session (if it exists).
pub async fn session_dir(session_id: &str) -> Option<PathBuf> {
    sessions()
        .lock()
        .await
        .get(session_id)
        .map(|s| s.dir.clone())
}

/// Check if the ffmpeg process for a session has exited with an error.
/// Returns the error message if it has, None if still running or session doesn't exist.
pub async fn session_error(session_id: &str) -> Option<String> {
    let map = sessions().lock().await;
    let session = map.get(session_id)?;
    session.exit_error.borrow().clone()
}

/// Stop and clean up a specific session.
pub async fn stop_session(session_id: &str) {
    sessions().lock().await.remove(session_id);
}

/// Clean up sessions that haven't been accessed in `max_idle_secs`.
/// Returns the number of sessions cleaned up.
pub async fn cleanup_idle(max_idle_secs: u64) -> usize {
    let mut map = sessions().lock().await;
    let now = Instant::now();
    let idle: Vec<String> = map
        .iter()
        .filter(|(_, s)| now.duration_since(s.last_access).as_secs() > max_idle_secs)
        .map(|(id, _)| id.clone())
        .collect();
    let count = idle.len();
    for id in idle {
        map.remove(&id);
    }
    count
}

/// Stop all sessions (for shutdown).
pub async fn stop_all() {
    sessions().lock().await.drain();
}
