//! Runs one ffmpeg pretranscode from a torrent stream into a cached MP4.
//! Progress is parsed from `-progress pipe:1`; the DB row is updated on the
//! same ~3s cadence as the download supervisor.
//!
//! On clean exit the `.mp4.part` is renamed to `.mp4` and the row goes
//! `completed`. Any other exit (ffmpeg error, cancel) removes the partial
//! file and records `failed` / `cancelled`.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::time::Duration;

use tokio::io::AsyncBufReadExt;

use crate::app::Pool;
use crate::downloads::TorrentEngine;

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
    info_hash: String,
    file_idx: i32,
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
        torrent_key: (String, i32),
        output_path: super::PretranscodingOutputPath,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Self {
        Self {
            pretranscoding_id,
            info_hash: torrent_key.0,
            file_idx: torrent_key.1,
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

        if let Err(err) = sqlx::query!(
            "UPDATE pretranscodings SET status = 'transcoding', error = NULL WHERE id = $1",
            self.pretranscoding_id,
        )
        .execute(&self.db)
        .await
        {
            tracing::error!(
                ?err,
                self.pretranscoding_id,
                "Failed to mark pretranscoding as transcoding"
            );
            return;
        }
        self.emit_status_update(PretranscodingStatus::Transcoding);

        let outcome = self.encode().await;

        self.finalize(outcome).await;
    }

    async fn encode(&self) -> EncodeOutcome {
        // Make sure the output directory exists.
        if let Some(parent) = self.output_path.parent()
            && let Err(err) = tokio::fs::create_dir_all(parent).await
        {
            return EncodeOutcome::Failed(format!("Failed to create output dir: {err}"));
        }

        let part_path = self.output_path.with_extension("mp4.part");
        // Remove any leftover partial from a previous run.
        let _ = tokio::fs::remove_file(&part_path).await;

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

        let mut command = crate::hls::ffmpeg::pretranscode(
            &self.config,
            self.output_path.only_audio,
            self.output_path.audio_index,
            &part_path,
        )
        .await;

        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(err) => return EncodeOutcome::Failed(format!("Failed to spawn ffmpeg: {err}")),
        };

        let Some(stdin) = child.stdin.take() else {
            return EncodeOutcome::Failed("ffmpeg stdin unavailable".into());
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

        // Feed the torrent stream into ffmpeg's stdin. Blocks on missing
        // pieces exactly like the live transcode's `write_task`.
        let engine = TorrentEngine::get();
        let reader = match engine.stream(&self.info_hash, self.file_idx as usize) {
            Ok(r) => r,
            Err(err) => {
                return EncodeOutcome::Failed(format!("Failed to open torrent stream: {err}"));
            }
        };
        let span = tracing::Span::current();
        let write_task = tokio::spawn(tracing::Instrument::instrument(
            async move {
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
            },
            span.clone(),
        ));

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
        let outcome = loop {
            tokio::select! {
                _ = self.cancel.cancelled() => {
                    break EncodeOutcome::Cancelled;
                }
                exit = child.wait() => {
                    let status = exit.ok();
                    match status {
                        Some(s) if s.success() && progress_ended.load(Ordering::Relaxed) == 1 => {
                            break EncodeOutcome::Completed {
                                total_ms: total_ms.or_else(|| {
                                    let us = out_time_us.load(Ordering::Relaxed);
                                    if us > 0 { Some(us / 1000) } else { None }
                                })
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
                    self.persist_progress(&out_time_us, total_ms).await;
                }
            }
        };

        // Kill ffmpeg and drain the reader tasks.
        // Bounded by per-task timeouts so a stuck reader can't block shutdown
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

        tokio::join!(
            child_timeout,
            Self::drain("write", write_task),
            Self::drain("progress", progress_task),
            Self::drain("stderr", stderr_task),
        );

        outcome
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
        let part_path = self.output_path.with_extension("mp4.part");
        match outcome {
            EncodeOutcome::Completed { total_ms } => {
                match tokio::fs::rename(&part_path, &self.output_path).await {
                    Ok(()) => {
                        let final_total = total_ms.unwrap_or(0);
                        if let Err(err) = sqlx::query!(
                            r#"
                                UPDATE pretranscodings
                                SET status = 'completed',
                                    completed_at = CURRENT_TIMESTAMP,
                                    transcoded_ms = $2,
                                    total_ms = COALESCE(total_ms, $2),
                                    error = NULL
                                WHERE id = $1
                            "#,
                            self.pretranscoding_id,
                            final_total,
                        )
                        .execute(&self.db)
                        .await
                        {
                            tracing::error!(
                                ?err,
                                self.pretranscoding_id,
                                "Failed to mark completed"
                            );
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
                        let msg = format!("Failed to finalize output: {err}");
                        self.mark_failed(&msg).await;
                        let _ = tokio::fs::remove_file(&part_path).await;
                    }
                }
            }
            EncodeOutcome::Cancelled => {
                let _ = tokio::fs::remove_file(&part_path).await;
                if let Err(err) = sqlx::query!(
                    "UPDATE pretranscodings SET status = 'cancelled' WHERE id = $1 AND status IN ('queued', 'transcoding')",
                    self.pretranscoding_id,
                )
                .execute(&self.db)
                .await
                {
                    tracing::warn!(?err, self.pretranscoding_id, "Failed to mark cancelled");
                }
                self.emit_status_update(PretranscodingStatus::Cancelled);
            }
            EncodeOutcome::Failed(msg) => {
                let _ = tokio::fs::remove_file(&part_path).await;
                self.mark_failed(&msg).await;
            }
        }
    }

    async fn mark_failed(&self, error: &str) {
        tracing::warn!(self.pretranscoding_id, "Pretranscode failed: {error}");
        if let Err(err) = sqlx::query!(
            "UPDATE pretranscodings SET status = 'failed', error = $1 WHERE id = $2 AND status NOT IN ('cancelled')",
            error,
            self.pretranscoding_id,
        )
        .execute(&self.db)
        .await
        {
            tracing::error!(?err, self.pretranscoding_id, "Failed to record failure");
        }
        self.emit_status_update(PretranscodingStatus::Failed);
    }

    async fn probe_duration_ms(&self) -> Option<i64> {
        let url = format!(
            "http://127.0.0.1:{}/api/stream/{}/{}",
            self.config.port, self.info_hash, self.file_idx
        );
        TorrentEngine::probe_duration(&url)
            .await
            .map(|s| (s * 1000.0) as i64)
    }

    async fn persist_progress(&self, out_time_us: &AtomicI64, total_ms: Option<i64>) {
        let us = out_time_us.load(Ordering::Relaxed);
        let ms = (us / 1000).max(0);
        if let Err(err) = sqlx::query!(
            "UPDATE pretranscodings SET transcoded_ms = $1 WHERE id = $2 AND status = 'transcoding'",
            ms,
            self.pretranscoding_id,
        )
        .execute(&self.db)
        .await
        {
            tracing::warn!(
                ?err,
                self.pretranscoding_id,
                "Failed to persist transcoded_ms"
            );
        }
        self.events
            .pretranscodings
            .emit_progress(&PretranscodingProgress {
                pretranscoding_id: self.pretranscoding_id,
                download_id: self.output_path.download_id,
                transcoded_ms: ms,
                total_ms,
                status: PretranscodingStatus::Transcoding,
                waiting_for_pieces: us == 0,
            });
    }

    fn emit_progress(&self, ms: i64, status: PretranscodingStatus) {
        self.events
            .pretranscodings
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
        self.events.pretranscodings.emit_status_update(
            &crate::api::pretranscodings::PretranscodingStatusUpdate {
                pretranscoding_id: self.pretranscoding_id,
                download_id: self.output_path.download_id,
                new_status,
            },
        );
    }
}

enum EncodeOutcome {
    Completed { total_ms: Option<i64> },
    Cancelled,
    Failed(String),
}
