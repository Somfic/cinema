//! Runs one ffmpeg pretranscode from a torrent stream into a cached MP4.
//! Progress is parsed from `-progress pipe:1`; the DB row is updated on the
//! same ~3s cadence as the download supervisor.
//!
//! Supports pause/resume via segmented output: each encode run writes
//! `.mp4.part.N`, and on final completion the segments are concat-copied
//! into the finished `.mp4` with `+faststart`. On soft stop (pause or live
//! eviction) ffmpeg receives SIGINT so it flushes a valid moov, and the row
//! keeps its `transcoded_ms` so the next run can resume via `-ss`.
//!
//! Soft vs hard cancel is signalled through the DB row: the manager sets
//! `paused`/`queued` (soft) or `cancelled` (hard) *before* firing the pool's
//! cancel token, and the supervisor reads that status to decide behavior.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::time::Duration;

use tokio::io::AsyncBufReadExt;

use crate::app::Pool;
use crate::downloads::{FfmpegInputSpec, MediaSource, TorrentEngine};

use super::types::PretranscodingStatus;

/// Emitted periodically while a pretranscode is running.
#[draad::ty]
pub struct PretranscodingProgress {
    pub pretranscoding_id: i32,
    pub download_id: i32,
    pub transcoded_ms: i64,
    pub total_ms: Option<i64>,
    pub status: PretranscodingStatus,
    /// True while ffmpeg hasn't emitted any encoded output yet — usually
    /// because it's blocked waiting for the head of the torrent to arrive.
    /// Lets the UI show "waiting for pieces" instead of a stuck 0%.
    pub waiting_for_pieces: bool,
}

pub struct Supervisor {
    pretranscoding_id: i32,
    source: MediaSource,
    output_path: super::PretranscodingOutputPath,
    db: Pool,
    events: crate::Events,
    config: Arc<crate::Config>,
    cancel: tokio_util::sync::CancellationToken,
}

impl Supervisor {
    pub fn new(
        db: Pool,
        events: crate::Events,
        config: Arc<crate::Config>,
        pretranscoding_id: i32,
        source: MediaSource,
        output_path: super::PretranscodingOutputPath,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Self {
        Self {
            pretranscoding_id,
            source,
            output_path,
            db,
            events,
            config,
            cancel,
        }
    }

    pub async fn run(self) {
        if self.cancel.is_cancelled() {
            return;
        }

        tracing::info!(
            self.pretranscoding_id,
            self.output_path.download_id,
            self.output_path.only_audio,
            self.output_path.audio_index,
            "Pretranscode supervisor started"
        );

        // Transition queued → transcoding and grab the resume checkpoint in
        // one shot. If no rows update, another supervisor beat us or the row
        // was moved out of `queued` (e.g. cancelled) before we got here.
        let resume_from_ms = match sqlx::query_scalar!(
            r#"
                UPDATE pretranscodings
                SET status = 'transcoding', error = NULL
                WHERE id = $1 AND status = 'queued'
                RETURNING transcoded_ms
            "#,
            self.pretranscoding_id,
        )
        .fetch_optional(&self.db)
        .await
        {
            Ok(Some(ms)) => ms,
            Ok(None) => return,
            Err(err) => {
                tracing::error!(
                    ?err,
                    self.pretranscoding_id,
                    "Failed to mark pretranscoding as transcoding"
                );
                return;
            }
        };

        self.emit_status_update(PretranscodingStatus::Transcoding);

        let outcome = self.encode(resume_from_ms).await;

        self.finalize(outcome).await;
    }

    async fn encode(&self, resume_from_ms: i64) -> EncodeOutcome {
        // Make sure the output directory exists.
        if let Some(parent) = self.output_path.parent()
            && let Err(err) = tokio::fs::create_dir_all(parent).await
        {
            return EncodeOutcome::Failed(format!("Failed to create output dir: {err}"));
        }

        // Pick the next segment index. Prior segments (if any) came from
        // previous pause/resume rounds and are kept for concat at completion.
        let existing = self.output_path.existing_segments().await;
        let next_idx = existing.last().map(|(i, _)| *i + 1).unwrap_or(0);
        let segment_path = self.output_path.segment(next_idx);
        // Defensive: a stale partial from a previous crashed run at this
        // index would otherwise contaminate the encode.
        let _ = tokio::fs::remove_file(&segment_path).await;

        // Total duration is best-effort. If probing fails, progress stays
        // indeterminate but the transcode still runs.
        let total_ms = self.probe_duration_ms().await;
        if let Some(total_ms) = total_ms
            && let Err(err) = sqlx::query!(
                "UPDATE pretranscodings SET total_ms = $1 WHERE id = $2",
                total_ms,
                self.pretranscoding_id,
            )
            .execute(&self.db)
            .await
        {
            tracing::warn!(?err, self.pretranscoding_id, "Failed to persist total_ms");
        }

        let copy_video = self.output_path.only_audio
            || crate::transcodings::probe::is_browser_safe(self.source.probe_path()).await;

        let start_time = (resume_from_ms as f64 / 1000.0).max(0.0);
        let mut command = crate::transcodings::ffmpeg::pretranscode(
            &self.config,
            &self.source,
            copy_video,
            self.output_path.audio_index,
            &segment_path,
            start_time,
        )
        .await;

        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(err) => return EncodeOutcome::Failed(format!("Failed to spawn ffmpeg: {err}")),
        };

        // Only Engine sources need stdin, Disk sources feed ffmpeg via
        // `-i <path>` and don't set up a piped stdin.
        let stdin_slot = if matches!(self.source.ffmpeg_input_spec(), FfmpegInputSpec::Pipe) {
            match child.stdin.take() {
                Some(s) => Some(s),
                None => return EncodeOutcome::Failed("ffmpeg stdin unavailable".into()),
            }
        } else {
            None
        };
        let Some(stdout) = child.stdout.take() else {
            return EncodeOutcome::Failed("ffmpeg stdout unavailable".into());
        };
        let Some(stderr) = child.stderr.take() else {
            return EncodeOutcome::Failed("ffmpeg stderr unavailable".into());
        };

        // Live counters written by the progress and stderr tasks, read by the
        // periodic DB writer.
        let out_time_us = Arc::new(AtomicI64::new(0));
        let progress_ended = Arc::new(AtomicUsize::new(0));
        let stderr_tail: Arc<tokio::sync::Mutex<Vec<String>>> =
            Arc::new(tokio::sync::Mutex::new(Vec::new()));

        // For Engine sources: spawn the stdin pump so ffmpeg blocks on missing
        // pieces exactly like the live transcode. For Disk: no pump since ffmpeg
        // reads the file itself via `-i <path>`.
        let span = tracing::Span::current();
        let write_task = if let Some(stdin) = stdin_slot {
            match self.source.spawn_stdin_pump(stdin).await {
                Ok(h) => Some(h),
                Err(err) => {
                    return EncodeOutcome::Failed(format!("Failed to open torrent stream: {err}"));
                }
            }
        } else {
            None
        };

        // Parse `-progress pipe:1` lines. Format is one `key=value` per line;
        // `out_time_us` is the value we care about. `progress=end` marks a
        // clean flush.
        let out_time_us_reader = out_time_us.clone();
        let progress_ended_reader = progress_ended.clone();
        let progress_task = tokio::spawn(tracing::Instrument::instrument(
            async move {
                let mut reader = tokio::io::BufReader::new(stdout);
                let mut line = String::new();
                loop {
                    line.clear();
                    match reader.read_line(&mut line).await {
                        Ok(0) => break,
                        Ok(_) => {
                            let trimmed = line.trim();
                            if let Some(v) = trimmed.strip_prefix("out_time_us=")
                                && let Ok(us) = v.parse::<i64>()
                            {
                                out_time_us_reader.store(us, Ordering::Relaxed);
                            } else if trimmed == "progress=end" {
                                progress_ended_reader.store(1, Ordering::Relaxed);
                            }
                        }
                        Err(_) => break,
                    }
                }
            },
            span.clone(),
        ));

        // Capture stderr for error context.
        let stderr_tail_writer = stderr_tail.clone();
        let stderr_task = tokio::spawn(tracing::Instrument::instrument(
            async move {
                let mut reader = tokio::io::BufReader::new(stderr);
                let mut line = String::new();
                loop {
                    line.clear();
                    match reader.read_line(&mut line).await {
                        Ok(0) => break,
                        Ok(_) => {
                            let trimmed = line.trim();
                            if !trimmed.is_empty() {
                                let mut tail = stderr_tail_writer.lock().await;
                                if tail.len() >= 5 {
                                    tail.remove(0);
                                }
                                tail.push(trimmed.to_string());
                            }
                        }
                        Err(_) => break,
                    }
                }
            },
            span.clone(),
        ));

        // Ticker loop: every 3s persist progress + emit event, check for
        // ffmpeg exit or cancellation.
        let mut interval = tokio::time::interval(Duration::from_secs(3));
        // Once we've sent SIGINT, disable the cancel branch and wait for the
        // child to flush its moov and exit via the wait branch.
        let mut stopping_soft = false;
        let outcome = loop {
            tokio::select! {
                _ = self.cancel.cancelled(), if !stopping_soft => {
                    if self.soft_stop_wanted().await {
                        if let Some(pid) = child.id() {
                            // SAFETY: `pid` came from a live child we still hold.
                            // A stale pid returning ESRCH here is harmless.
                            unsafe { libc::kill(pid as i32, libc::SIGINT); }
                        }
                        stopping_soft = true;
                    } else {
                        Self::drain("progress", progress_task).await;
                        break EncodeOutcome::HardCancelled;
                    }
                }
                exit = child.wait() => {
                    Self::drain("progress", progress_task).await;
                    let us = out_time_us.load(Ordering::Relaxed).max(0);
                    let absolute_ms = resume_from_ms + us / 1000;

                    if stopping_soft {
                        break EncodeOutcome::SoftStopped { absolute_ms };
                    }

                    match exit.ok() {
                        Some(s) if s.success() && progress_ended.load(Ordering::Relaxed) == 1 => {
                            let derived_total = total_ms.or(if absolute_ms > 0 {
                                Some(absolute_ms)
                            } else {
                                None
                            });
                            break EncodeOutcome::Completed {
                                total_ms: derived_total,
                                absolute_ms,
                            };
                        }
                        _ => {
                            let tail = stderr_tail.lock().await.join("\n");
                            break EncodeOutcome::Failed(if tail.is_empty() {
                                "ffmpeg exited without progress=end".into()
                            } else {
                                tail
                            });
                        }
                    }
                }
                _ = interval.tick() => {
                    self.persist_progress(&out_time_us, resume_from_ms, total_ms).await;
                }
            }
        };

        // Kill ffmpeg (idempotent if it already exited) and drain remaining
        // reader tasks. Bounded so a stuck reader can't hang shutdown.
        let child_timeout = async {
            if let Err(err) = tokio::time::timeout(Duration::from_secs(1), async {
                if let Err(err) = child.kill().await {
                    tracing::warn!(?err, "Could not kill the preprocessing child process!");
                }
            })
            .await
            {
                tracing::warn!(
                    ?err,
                    "Killing preprocessing ffmpeg child process timed out!"
                );
            }
        };

        let drain_write = async {
            if let Some(task) = write_task {
                Self::drain("write", task).await;
            }
        };

        tokio::join!(
            child_timeout,
            drain_write,
            Self::drain("stderr", stderr_task),
        );

        outcome
    }

    /// True iff the current DB status says "keep the segment on stop":
    /// either `paused` (user pause) or `queued` (live eviction rewinds to
    /// queued before firing cancel). Any other status (`cancelled`,
    /// `transcoding`, row missing) means a hard cancel.
    async fn soft_stop_wanted(&self) -> bool {
        let res = sqlx::query_scalar!(
            r#"SELECT EXISTS (SELECT 1 FROM pretranscodings WHERE id = $1 AND status IN ('queued', 'paused')) as "exists!: bool""#,
            self.pretranscoding_id
        ).fetch_one(&self.db).await;

        match res {
            Ok(exists) => exists,
            Err(err) => {
                tracing::warn!(
                    ?err,
                    "Error checking pretranscoding status. Falling back to hard stop"
                );

                false
            }
        }
    }

    async fn drain(name: &'static str, handle: tokio::task::JoinHandle<()>) {
        if let Err(err) = tokio::time::timeout(Duration::from_secs(1), async {
            handle.abort();
            if let Err(err) = handle.await
                && !err.is_cancelled()
            {
                tracing::warn!(?err, "Awaiting {name} task returned an error!");
            }
        })
        .await
        {
            tracing::warn!(?err, "Aborting the {name} task timed out!");
        }
    }

    async fn finalize(&self, outcome: EncodeOutcome) {
        match outcome {
            EncodeOutcome::Completed {
                total_ms,
                absolute_ms,
            } => match self.concat_segments().await {
                Ok(()) => {
                    let final_total = total_ms.unwrap_or(absolute_ms);
                    if let Err(err) = sqlx::query!(
                        r#"
                            UPDATE pretranscodings
                            SET status = 'completed',
                                completed_at = CURRENT_TIMESTAMP,
                                transcoded_ms = $2,
                                total_ms = COALESCE(total_ms, $2),
                                error = NULL
                            WHERE id = $1 AND status = 'transcoding'
                        "#,
                        self.pretranscoding_id,
                        final_total,
                    )
                    .execute(&self.db)
                    .await
                    {
                        tracing::error!(?err, self.pretranscoding_id, "Failed to mark completed");
                    }
                    self.emit_progress(final_total, PretranscodingStatus::Completed);
                    self.emit_status_update(PretranscodingStatus::Completed);
                    tracing::info!(
                        self.pretranscoding_id,
                        path = %self.output_path.display(),
                        "Pretranscode completed"
                    );
                }
                Err(err) => {
                    self.output_path.remove_all_segments().await;
                    self.mark_failed(&format!("Failed to concat segments: {err}"))
                        .await;
                }
            },
            EncodeOutcome::SoftStopped { absolute_ms } => {
                // Persist the checkpoint. Status is already `paused` (user
                // pause) or `queued` (eviction): the WHERE clause guards
                // against a racing hard cancel that flipped us to `cancelled`
                // between the cancel token firing and finalize running.
                let res = sqlx::query!(
                    "UPDATE pretranscodings SET transcoded_ms = $1 WHERE id = $2 AND status IN ('paused', 'queued')",
                    absolute_ms,
                    self.pretranscoding_id,
                )
                .execute(&self.db)
                .await;
                if !matches!(&res, Ok(r) if r.rows_affected() > 0) {
                    // Status moved away from paused/queued during shutdown
                    // (probably cancelled). Segments would be dead weight.
                    self.output_path.remove_all_segments().await;
                }
            }
            EncodeOutcome::HardCancelled => {
                self.output_path.remove_all_segments().await;
                // Manager set `cancelled` before firing the token; only emit
                // if we're the one flipping it (i.e. a bug path where the row
                // is still `transcoding`).
                let res = sqlx::query!(
                    "UPDATE pretranscodings SET status = 'cancelled' WHERE id = $1 AND status = 'transcoding'",
                    self.pretranscoding_id,
                )
                .execute(&self.db)
                .await;
                if matches!(&res, Ok(r) if r.rows_affected() > 0) {
                    self.emit_status_update(PretranscodingStatus::Cancelled);
                }
            }
            EncodeOutcome::Failed(msg) => {
                self.output_path.remove_all_segments().await;
                self.mark_failed(&msg).await;
            }
        }
    }

    /// Concat-copy all `.mp4.part.*` segments into the final `.mp4` with
    /// `+faststart`. Precondition: every segment was produced by the same
    /// `VideoPipeline`, so codec params match and `-c copy` works.
    async fn concat_segments(&self) -> Result<(), String> {
        let segments = self.output_path.existing_segments().await;
        if segments.is_empty() {
            return Err("No segments to concat".into());
        }

        let list_path = self.output_path.with_extension("mp4.concat");
        // The concat demuxer resolves `file` entries relative to the concat
        // list's own directory. Since segments live next to the list, use
        // basenames; absolute paths would work too, but basenames don't
        // leak the storage prefix into the file.
        let list_body: String = segments
            .iter()
            .filter_map(|(_, p)| p.file_name().and_then(|n| n.to_str()))
            .map(|name| {
                // Concat demuxer's `file` directive: escape single-quotes.
                let escaped = name.replace('\'', r"'\''");
                format!("file '{escaped}'\n")
            })
            .collect();
        if let Err(err) = tokio::fs::write(&list_path, list_body).await {
            return Err(format!("Write concat list: {err}"));
        }

        let mut cmd = crate::transcodings::ffmpeg::concat(&list_path, &self.output_path);
        let output = cmd.output().await;
        let _ = tokio::fs::remove_file(&list_path).await;

        let output = output.map_err(|err| format!("Wait concat: {err}"))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let tail: Vec<&str> = stderr.lines().rev().take(5).collect();
            let msg: Vec<&str> = tail.into_iter().rev().collect();
            return Err(format!(
                "Concat exit {}: {}",
                output.status,
                msg.join(" | ")
            ));
        }

        self.output_path.remove_all_segments().await;
        Ok(())
    }

    async fn mark_failed(&self, error: &str) {
        tracing::warn!(self.pretranscoding_id, "Pretranscode failed: {error}");
        let res = sqlx::query!(
            "UPDATE pretranscodings SET status = 'failed', error = $1 WHERE id = $2 AND status NOT IN ('queued', 'cancelled', 'paused')",
            error,
            self.pretranscoding_id,
        )
        .execute(&self.db)
        .await;
        match res {
            Ok(res) if res.rows_affected() > 0 => {
                self.emit_status_update(PretranscodingStatus::Failed);
            }
            Ok(_) => {}
            Err(err) => {
                tracing::error!(?err, self.pretranscoding_id, "Failed to record failure");
            }
        };
    }

    async fn probe_duration_ms(&self) -> Option<i64> {
        let input = self.source.coherent_input(&self.config);
        TorrentEngine::probe_duration(&input)
            .await
            .filter(|s| !s.is_infinite() && *s > 0.0)
            .map(|s| (s * 1000.0) as i64)
    }

    async fn persist_progress(
        &self,
        out_time_us: &AtomicI64,
        resume_from_ms: i64,
        total_ms: Option<i64>,
    ) {
        let us = out_time_us.load(Ordering::Relaxed);
        let absolute_ms = resume_from_ms + (us / 1000).max(0);
        let res = sqlx::query!(
            "UPDATE pretranscodings SET transcoded_ms = $1 WHERE id = $2 AND status = 'transcoding'",
            absolute_ms,
            self.pretranscoding_id,
        )
        .execute(&self.db)
        .await;

        match res {
            Ok(r) if r.rows_affected() > 0 => {
                self.events
                    .transcodings
                    .emit_progress(&PretranscodingProgress {
                        pretranscoding_id: self.pretranscoding_id,
                        download_id: self.output_path.download_id,
                        transcoded_ms: absolute_ms,
                        total_ms,
                        status: PretranscodingStatus::Transcoding,
                        waiting_for_pieces: us == 0,
                    });
            }
            Ok(_) => {
                // Status moved out of `transcoding` (paused, evicted, cancelled).
                // Suppress the tick so the UI doesn't briefly flip back.
            }
            Err(err) => {
                tracing::warn!(
                    ?err,
                    self.pretranscoding_id,
                    "Failed to persist transcoded_ms"
                );
            }
        }
    }

    fn emit_progress(&self, ms: i64, status: PretranscodingStatus) {
        self.events
            .transcodings
            .emit_progress(&PretranscodingProgress {
                pretranscoding_id: self.pretranscoding_id,
                download_id: self.output_path.download_id,
                transcoded_ms: ms,
                total_ms: None,
                status,
                waiting_for_pieces: false,
            });
    }

    fn emit_status_update(&self, new_status: PretranscodingStatus) {
        self.events.transcodings.emit_status_update(
            &crate::api::transcodings::PretranscodingStatusUpdate {
                pretranscoding_id: self.pretranscoding_id,
                download_id: self.output_path.download_id,
                new_status,
            },
        );
    }
}

enum EncodeOutcome {
    Completed {
        total_ms: Option<i64>,
        absolute_ms: i64,
    },
    /// User-initiated pause or live eviction: keep segments, persist checkpoint.
    SoftStopped {
        absolute_ms: i64,
    },
    /// User cancel / remove: discard segments, mark cancelled.
    HardCancelled,
    Failed(String),
}
