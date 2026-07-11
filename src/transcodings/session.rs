//! Live HLS session state and its ffmpeg-driving glue.
//!
//! A [`LiveSession`] is a running ffmpeg child plus its temp segment
//! directory, held in the manager's in-memory map. Dropping the session
//! kills the process, aborts the reader tasks, and asynchronously removes
//! the directory.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::watch;

use crate::downloads::{FfmpegInputSpec, MediaSource};

pub(super) struct LiveSession {
    pub(super) dir: PathBuf,
    pub(super) last_access: Instant,
    /// The [`SupervisorPool`] key that reserves this session's capacity slot.
    /// Live sessions use negative i32s to avoid colliding with pretranscoding
    /// row IDs (Postgres SERIAL, always positive).
    ///
    /// [`SupervisorPool`]: crate::utils::supervisor_pool::SupervisorPool
    pub(super) pool_id: i32,
    /// Receives the ffmpeg error message when the process exits with failure.
    /// `None` means still running, `Some(msg)` means exited with that error.
    pub(super) exit_error: watch::Receiver<Option<String>>,
    /// First handle is the monitor task that owns the ffmpeg `Child`.
    /// Aborting it drops the child, which kills the process via
    /// `kill_on_drop`.
    pub(super) abort_handles: Vec<tokio::task::AbortHandle>,
}

impl Drop for LiveSession {
    fn drop(&mut self) {
        for handle in &self.abort_handles {
            handle.abort();
        }
        let dir = std::mem::take(&mut self.dir);
        tokio::spawn(async move {
            let _ = tokio::fs::remove_dir_all(&dir).await;
        });
    }
}

/// Random 16-char hex session ID. Mixes a wall-clock nanosecond timestamp
/// with a stack address so two sessions starting in the same nanosecond
/// still get distinct IDs.
pub(super) fn new_session_id() -> String {
    use rand::Rng;

    let mut bytes = [0u8; 8];
    rand::rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// Spawn an ffmpeg live-transcode child and wire its stdin pump (for Engine
/// sources), stderr tail reader, and exit-error watch. Returns the
/// [`LiveSession`] holding the process; the caller inserts it into the
/// manager's session map and then awaits `wait_for_playlist_ready`.
///
/// `source` is `Some` only for the fresh-transcode path; the cache-hit
/// (local_transcode) path reads from disk and doesn't need a pump.
pub(super) async fn spawn_live_ffmpeg(
    mut command: tokio::process::Command,
    source: Option<&MediaSource>,
    session_id: &str,
    dir: PathBuf,
    pool_id: i32,
    input_display: String,
) -> crate::app::Result<LiveSession> {
    let mut child = command
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| crate::app::Error::Generic(format!("Failed to start ffmpeg HLS: {e}")))?;

    // For Engine sources we need to pump bytes into ffmpeg's stdin (so it
    // blocks on missing pieces rather than hitting EOF). Disk sources use
    // `-i <path>` and don't need a pump task.
    let write_task = if let Some(source) = source
        && matches!(source.ffmpeg_input_spec(), FfmpegInputSpec::Pipe)
    {
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| crate::app::Error::Generic("Failed to open ffmpeg stdin".into()))?;
        Some(source.spawn_stdin_pump(stdin).await?)
    } else {
        None
    };

    let (exit_tx, exit_rx) = watch::channel(None);
    let stderr = child.stderr.take();
    let sid = session_id.to_string();
    let span = tracing::Span::current();

    // The monitor task owns the child so it can inspect the actual exit
    // status after stderr closes. `kill_on_drop(true)` on the command means
    // that aborting this task drops the child and sends SIGKILL.
    let monitor_task = tokio::spawn(tracing::Instrument::instrument(
        async move {
            let mut last_lines: VecDeque<String> = VecDeque::new();

            if let Some(stderr) = stderr {
                let mut reader = BufReader::new(stderr);
                let mut line = String::new();
                loop {
                    line.clear();
                    match reader.read_line(&mut line).await {
                        Ok(0) => break,
                        Ok(_) => {
                            let trimmed = line.trim();
                            if !trimmed.is_empty() {
                                tracing::trace!(session = %sid, "ffmpeg: {trimmed}");
                                if last_lines.len() >= 5 {
                                    last_lines.pop_front();
                                }
                                last_lines.push_back(trimmed.to_string());
                            }
                        }
                        Err(e) => {
                            tracing::warn!(session = %sid, "ffmpeg stderr read error: {e}");
                            break;
                        }
                    }
                }
            }

            let status = match child.wait().await {
                Ok(s) => s,
                Err(e) => {
                    let msg = format!("Failed to wait for ffmpeg: {e}");
                    tracing::warn!(session = %sid, input = %input_display, "{msg}");
                    let _ = exit_tx.send(Some(msg));
                    return;
                }
            };

            if status.success() {
                tracing::info!(session = %sid, input = %input_display, "ffmpeg finished successfully");
            } else {
                let tail = if last_lines.is_empty() {
                    "(no stderr output)".to_string()
                } else {
                    Vec::from(last_lines).join("\n")
                };
                let msg = format!("ffmpeg exited with {status}: {tail}");
                tracing::warn!(session = %sid, input = %input_display, "ffmpeg failed: {msg}");
                let _ = exit_tx.send(Some(msg));
            }
        },
        span,
    ));

    let mut abort_handles = vec![monitor_task.abort_handle()];
    if let Some(ref w) = write_task {
        abort_handles.push(w.abort_handle());
    }

    Ok(LiveSession {
        dir,
        last_access: Instant::now(),
        pool_id,
        exit_error: exit_rx,
        abort_handles,
    })
}

/// Poll for the ffmpeg-produced playlist file to contain at least one
/// segment, or bail out on startup timeout / ffmpeg failure. Used by the
/// manager right after inserting a fresh `LiveSession` into the map, so
/// clients only see the URL once the first segment is on disk.
pub(super) async fn wait_for_playlist_ready(
    playlist_path: &Path,
    exit_error: &mut watch::Receiver<Option<String>>,
    max_startup: std::time::Duration,
    poll_interval: std::time::Duration,
) -> crate::app::Result<()> {
    let result = tokio::time::timeout(max_startup, async {
        loop {
            if let Some(error) = exit_error.borrow().clone() {
                return Err(crate::app::Error::Generic(format!(
                    "ffmpeg failed: {error}"
                )));
            }

            if let Ok(metadata) = tokio::fs::metadata(playlist_path).await
                && metadata.len() > 0
                && let Ok(content) = tokio::fs::read_to_string(playlist_path).await
                && content.contains("#EXTINF")
            {
                return Ok(());
            }
            tokio::time::sleep(poll_interval).await;
        }
    })
    .await;

    match result {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(crate::app::Error::Generic(String::from(
            "ffmpeg startup timeout",
        ))),
    }
}

/// Live-session state guarded by an async mutex. Cheap to clone; multiple
/// [`Handle`]s share the same underlying map.
///
/// [`Handle`]: crate::transcodings::Handle
pub(super) type SessionMap =
    Arc<tokio::sync::Mutex<std::collections::HashMap<String, LiveSession>>>;
