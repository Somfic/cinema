use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use tokio::sync::Mutex;
use tokio::sync::watch;

static SESSIONS: OnceLock<Mutex<HashMap<String, HlsSession>>> = OnceLock::new();

use std::sync::OnceLock;

struct HlsSession {
    dir: PathBuf,
    child: tokio::process::Child,
    last_access: Instant,
    /// Receives the ffmpeg error message when the process exits with failure.
    /// `None` means still running, `Some(msg)` means exited with that error.
    exit_error: watch::Receiver<Option<String>>,
    abort_handles: [tokio::task::AbortHandle; 2],
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

/// Encoder keyframe interval (in frames) for transcoded video. ~2s at 24-30fps,
/// matching the 2s HLS segment length so the muxer always has a cut point.
const GOP_FRAMES: u32 = 48;

/// Probe the video codec of a file using ffprobe.
pub async fn probe_video_codec(path: &std::path::Path) -> Option<String> {
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

/// Whether the `h264_nvenc` hardware encoder is usable (compiled in AND backed
/// by a working GPU). Probed once via a trivial test encode and cached, since a
/// machine whose ffmpeg lists nvenc may still lack an NVIDIA device.
static NVENC_AVAILABLE: tokio::sync::OnceCell<bool> = tokio::sync::OnceCell::const_new();

async fn nvenc_available() -> bool {
    *NVENC_AVAILABLE
        .get_or_init(|| async {
            let status = tokio::process::Command::new("ffmpeg")
                .args([
                    "-hide_banner",
                    "-loglevel",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    "nullsrc=s=256x256:d=0.1",
                    "-c:v",
                    "h264_nvenc",
                    "-f",
                    "null",
                    "-",
                ])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .await;
            let ok = matches!(status, Ok(s) if s.success());
            if ok {
                tracing::info!("h264_nvenc hardware encoder available");
            } else {
                tracing::info!("h264_nvenc unavailable; falling back to software libx264");
            }
            ok
        })
        .await
}

/// The video half of an ffmpeg invocation, split by where each arg must go.
struct VideoPipeline {
    /// Decode-side args that must precede `-i` (e.g. `-hwaccel`).
    pre_input: Vec<String>,
    /// Output-side filter args (e.g. `-vf scale_cuda=...`).
    filter: Vec<String>,
    /// Encoder selection + tuning (`-c:v ...`).
    encode: Vec<String>,
}

/// Decide how to handle the video stream. `copy_video` streams it through
/// untouched; otherwise we prefer a fully GPU-resident NVENC pipeline (decode →
/// scale_cuda → encode, no CPU frame copy) and fall back to software libx264.
///
/// The NVENC path matters for load: with `-hwaccel auto` ffmpeg decodes on the
/// GPU but downloads every frame to do the 10-bit→8-bit `-pix_fmt` conversion on
/// the CPU (~4 cores for 4K). Keeping frames on the GPU via `scale_cuda` drops
/// that to a fraction.
async fn video_pipeline(config: &crate::Config, copy_video: bool) -> VideoPipeline {
    if copy_video {
        return VideoPipeline {
            pre_input: Vec::new(),
            filter: Vec::new(),
            encode: vec!["-c:v".into(), "copy".into()],
        };
    }

    let want_nvenc = matches!(config.ffmpeg_hwaccel.as_str(), "auto" | "cuda" | "nvenc");
    if want_nvenc && nvenc_available().await {
        VideoPipeline {
            // Decode on the GPU and keep frames in VRAM.
            pre_input: vec![
                "-hwaccel".into(),
                "cuda".into(),
                "-hwaccel_output_format".into(),
                "cuda".into(),
            ],
            // Convert 10-bit (p010) → 8-bit (nv12) on the GPU; h264_nvenc is
            // 8-bit only. Doing it here avoids the CPU `-pix_fmt` round-trip.
            filter: vec!["-vf".into(), "scale_cuda=format=nv12".into()],
            encode: vec![
                "-c:v".into(),
                "h264_nvenc".into(),
                "-preset".into(),
                "p4".into(),
                "-tune".into(),
                "ll".into(), // low-latency: prioritise fast segment production
                "-rc".into(),
                "vbr".into(),
                "-cq".into(),
                config.ffmpeg_video_crf.to_string(),
                "-b:v".into(),
                "0".into(),
                // Fixed keyframe interval (~2s at 24-30fps). Without it, nvenc
                // with `-tune ll` emits only the opening keyframe and *ignores*
                // `-force_key_frames`, so the HLS muxer finds no split points,
                // buffers everything into segment 0, and only flushes at EOF —
                // read as a startup timeout. A periodic IDR lets segments cut.
                "-g".into(),
                GOP_FRAMES.to_string(),
            ],
        }
    } else {
        VideoPipeline {
            pre_input: if config.ffmpeg_hwaccel != "none" {
                vec!["-hwaccel".into(), config.ffmpeg_hwaccel.clone()]
            } else {
                Vec::new()
            },
            filter: Vec::new(),
            encode: vec![
                "-c:v".into(),
                "libx264".into(),
                "-preset".into(),
                config.ffmpeg_video_preset.clone(),
                "-tune".into(),
                "zerolatency".into(),
                "-crf".into(),
                config.ffmpeg_video_crf.to_string(),
                // Match the segment cadence so the first segment closes quickly
                // rather than at libx264's default ~250-frame keyint.
                "-g".into(),
                GOP_FRAMES.to_string(),
                "-pix_fmt".into(),
                "yuv420p".into(),
                "-bf".into(),
                "0".into(),
                "-b_strategy".into(),
                "0".into(),
            ],
        }
    }
}

pub struct HlsSessionStartInput {
    pub info_hash: String,
    pub file_idx: usize,
    pub audio_index: usize,
    pub start_time: f64,
    pub only_audio: bool,
}

/// Start an HLS remux session. Returns (session_id, playlist_path).
/// Spawns ffmpeg reading from a torrent stream (blocks on missing pieces)
/// and writing HLS segments to a temp directory.
/// If `start_time` > 0, ffmpeg seeks to that position before encoding.
pub async fn start_session(
    storage: &crate::app::Storage,
    config: &crate::Config,
    session_input: HlsSessionStartInput,
) -> crate::app::Result<(String, String)> {
    let session_id = new_session_id();
    let dir = storage.join(format!("hls/{session_id}"));
    tokio::fs::create_dir_all(&dir).await?;

    match start_transcoding(config, session_input, &session_id, dir.clone()).await {
        Ok(url) => Ok((session_id, url)),
        Err(err) => {
            let _ = tokio::fs::remove_dir_all(dir).await;
            Err(err)
        }
    }
}

async fn start_transcoding(
    config: &crate::Config,
    session_input: HlsSessionStartInput,
    session_id: &String,
    dir: PathBuf,
) -> crate::app::Result<String> {
    let playlist_path = dir.join("playlist.m3u8");
    let segment_pattern = dir.join("seg%05d.ts");

    let engine = crate::downloads::TorrentEngine::get();
    let file_path = engine.file_path(&session_input.info_hash, session_input.file_idx)?;
    let video_codec = probe_video_codec(&file_path).await.unwrap_or_default();
    let copy_video =
        session_input.only_audio || BROWSER_SAFE_VIDEO.iter().any(|c| video_codec.contains(c));

    let video = video_pipeline(config, copy_video).await;

    // Decode-side args precede `-i`: the video hwaccel, then input seeking.
    let mut pre_args = video.pre_input.clone();
    if session_input.start_time > 0.0 {
        pre_args.extend_from_slice(&[
            "-ss".into(),
            format!("{:.3}", session_input.start_time),
            "-copyts".into(),
        ]);
    }

    let input_display = format!(
        "torrent:{}/{}",
        session_input.info_hash, session_input.file_idx
    );

    // Feed ffmpeg from stdin using the torrent stream, which blocks on
    // missing pieces rather than hitting premature EOF on a partial file.
    let mut child = tokio::process::Command::new("ffmpeg")
        .args(&pre_args)
        .args([
            "-i",
            "pipe:0",
            "-map",
            "0:v:0",
            "-map",
            &format!("0:a:{}", session_input.audio_index),
        ])
        .args(&video.filter)
        .args(&video.encode)
        .args([
            "-c:a",
            "aac",
            "-b:a",
            "192k",
            "-ac",
            "2",
            "-af",
            "aresample=async=1:first_pts=0",
            "-f",
            "hls",
            // Short segments so the first one needs far less input data — critical
            // for a cold torrent (have 0) where ffmpeg blocks on pipe:0 waiting for
            // the head of the file to download before it can emit segment 0.
            "-hls_time",
            "2",
            "-hls_list_size",
            "0",
            "-hls_flags",
            "append_list",
            "-hls_segment_filename",
            segment_pattern.to_str().unwrap_or(""),
            "-hls_playlist_type",
            "event",
        ])
        .arg(playlist_path.to_str().unwrap_or(""))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| crate::app::Error::Generic(format!("Failed to start ffmpeg HLS: {e}")))?;

    // Pipe the torrent stream into ffmpeg's stdin
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| crate::app::Error::Generic("Failed to open ffmpeg stdin".into()))?;

    let engine = crate::downloads::TorrentEngine::get();
    let reader = engine.stream(&session_input.info_hash, session_input.file_idx)?;

    let span = tracing::Span::current();

    let write_task = tokio::spawn(async move {
        use tokio::io::AsyncReadExt;

        let mut reader = reader;
        let mut stdin = stdin;
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            match reader.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    use tokio::io::AsyncWriteExt;
                    if stdin.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    // Capture stderr and track process exit
    let (exit_tx, exit_rx) = watch::channel(None);
    let stderr = child.stderr.take();
    let sid = session_id.clone();

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

    sessions().lock().await.insert(
        session_id.clone(),
        HlsSession {
            dir,
            child,
            last_access: Instant::now(),
            exit_error: exit_rx,
            abort_handles: [write_task.abort_handle(), error_task.abort_handle()],
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
