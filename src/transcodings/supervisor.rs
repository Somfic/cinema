//! Runs one background pretranscode into a cached MP4 that plays directly in
//! a browser or on a Chromecast. Progress is persisted on the same ~3s
//! cadence as the download supervisor.
//!
//! The job runs in-process, so a soft stop (user pause, or eviction by a live
//! stream) suspends it where it is and parks it with the manager; resuming
//! picks the parked job back up. Parked jobs don't survive a restart: the
//! row then starts over from the beginning.
//!
//! Soft vs hard cancel is signalled through the DB row: the manager sets
//! `paused`/`queued` (soft) or `cancelled` (hard) *before* firing the pool's
//! cancel token, and the supervisor reads that status to decide behavior.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::app::Pool;
use crate::downloads::MediaSource;

use super::PretranscodingOutputPath;
use super::types::PretranscodingStatus;

/// Emitted periodically while a pretranscode is running.
#[draad::ty]
pub struct PretranscodingProgress {
    pub pretranscoding_id: i32,
    pub download_id: i32,
    pub transcoded_ms: i64,
    pub total_ms: Option<i64>,
    pub status: PretranscodingStatus,
    /// True while nothing has been transcoded yet - usually because the
    /// head of the torrent hasn't arrived. Lets the UI show "waiting for
    /// pieces" instead of a stuck 0%.
    pub waiting_for_pieces: bool,
}

/// A suspended job, waiting for its slot back.
pub(super) struct Parked {
    job: media::file::FileTranscode,
    total_ms: Option<i64>,
}

pub(super) type ParkedJobs = Arc<Mutex<HashMap<i32, Parked>>>;

pub(super) enum Start {
    Fresh {
        source: MediaSource,
        only_audio: bool,
        audio_index: i32,
    },
    Parked(Parked),
}

pub struct Supervisor {
    pub(super) pretranscoding_id: i32,
    pub(super) output: PretranscodingOutputPath,
    pub(super) db: Pool,
    pub(super) events: crate::Events,
    pub(super) config: Arc<crate::Config>,
    pub(super) parked: ParkedJobs,
    pub(super) cancel: tokio_util::sync::CancellationToken,
}

impl Supervisor {
    pub(super) async fn run(self, start: Start) {
        if self.cancel.is_cancelled() {
            if let Start::Parked(parked) = start {
                self.parked
                    .lock()
                    .unwrap()
                    .insert(self.pretranscoding_id, parked);
            }
            return;
        }

        // Transition queued → transcoding. If no rows update, the row was
        // moved out of `queued` (e.g. cancelled) before we got here.
        let fresh = matches!(start, Start::Fresh { .. });
        let claimed = sqlx::query!(
            r#"
                UPDATE pretranscodings
                SET status = 'transcoding',
                    error = NULL,
                    transcoded_ms = CASE WHEN $2 THEN 0 ELSE transcoded_ms END
                WHERE id = $1 AND status = 'queued'
            "#,
            self.pretranscoding_id,
            fresh,
        )
        .execute(&self.db)
        .await;
        match claimed {
            Ok(r) if r.rows_affected() > 0 => {}
            Ok(_) => return,
            Err(err) => {
                tracing::error!(
                    ?err,
                    self.pretranscoding_id,
                    "Failed to mark pretranscoding as transcoding"
                );
                return;
            }
        }

        tracing::info!(
            self.pretranscoding_id,
            self.output.download_id,
            self.output.only_audio,
            self.output.audio_index,
            resumed = !fresh,
            "Pretranscode supervisor started"
        );
        self.emit_status_update(PretranscodingStatus::Transcoding);

        let parked = match start {
            Start::Parked(parked) => {
                parked.job.resume();
                parked
            }
            Start::Fresh {
                source,
                only_audio,
                audio_index,
            } => match self.begin(&source, only_audio, audio_index).await {
                Ok(parked) => parked,
                Err(err) => {
                    self.mark_failed(&err.to_string()).await;
                    return;
                }
            },
        };

        self.supervise(parked).await;
    }

    async fn begin(
        &self,
        source: &MediaSource,
        only_audio: bool,
        audio_index: i32,
    ) -> crate::app::Result<Parked> {
        let input = source.media_input().await?;
        let info = Arc::new(media::probe(&input).await?);
        let total_ms = info.duration.map(|d| d.as_millis() as i64);
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

        // The output has to play everywhere: H.264 video and AAC audio.
        // "Only audio" keeps the video as it is.
        let universal = media::ClientCaps {
            video_codecs: vec!["h264".into()],
            audio_codecs: vec!["aac".into()],
            ..Default::default()
        };
        let plan = media::plan(
            &info,
            &universal,
            &media::PlanRequest {
                audio_index: audio_index.max(0) as usize,
                ..Default::default()
            },
        );
        let (mut video, audio) = match plan {
            media::Plan::Hls { video, audio } => (video, audio),
            media::Plan::Direct => (media::VideoAction::Copy, media::AudioAction::Copy),
        };
        let copyable = info
            .video
            .first()
            .is_some_and(|v| matches!(v.codec.as_str(), "h264" | "hevc" | "av1" | "vp9"));
        if only_audio && copyable {
            video = media::VideoAction::Copy;
        }

        let job = media::file::FileTranscode::start(media::file::TranscodeOptions {
            input,
            info,
            video,
            audio,
            audio_index: audio_index.max(0) as usize,
            encoder: self.config.encoder(),
            output: self.output.to_path_buf(),
        })?;
        Ok(Parked { job, total_ms })
    }

    async fn supervise(self, parked: Parked) {
        let mut interval = tokio::time::interval(Duration::from_secs(3));
        let outcome = loop {
            tokio::select! {
                _ = self.cancel.cancelled() => {
                    if self.soft_stop_wanted().await {
                        parked.job.pause();
                        self.checkpoint(&parked).await;
                        tracing::info!(self.pretranscoding_id, "Pretranscode suspended");
                        self.parked.lock().unwrap().insert(self.pretranscoding_id, parked);
                        return;
                    }
                    break Err("cancelled".to_string());
                }
                result = parked.job.wait() => break result.map_err(|e| e.to_string()),
                _ = interval.tick() => self.persist_progress(&parked).await,
            }
        };

        match outcome {
            Ok(()) => {
                let final_ms = parked
                    .total_ms
                    .or(parked.job.position().map(|p| p.as_millis() as i64))
                    .unwrap_or(0);
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
                    final_ms,
                )
                .execute(&self.db)
                .await
                {
                    tracing::error!(?err, self.pretranscoding_id, "Failed to mark completed");
                }
                self.emit_progress(final_ms, parked.total_ms, PretranscodingStatus::Completed);
                self.emit_status_update(PretranscodingStatus::Completed);
                tracing::info!(
                    self.pretranscoding_id,
                    path = %self.output.display(),
                    "Pretranscode completed"
                );
            }
            Err(err) if self.cancel.is_cancelled() => {
                // Dropping the job removes its partial output. The manager set
                // `cancelled` before firing the token; only emit if we're the
                // one flipping it.
                drop(parked);
                tracing::debug!(self.pretranscoding_id, "Pretranscode stopped: {err}");
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
            Err(err) => {
                drop(parked);
                self.mark_failed(&err).await;
            }
        }
    }

    /// True iff the current DB status says "suspend, don't discard": either
    /// `paused` (user pause) or `queued` (live eviction rewinds to queued
    /// before firing cancel). Anything else is a hard cancel.
    async fn soft_stop_wanted(&self) -> bool {
        let res = sqlx::query_scalar!(
            r#"SELECT EXISTS (SELECT 1 FROM pretranscodings WHERE id = $1 AND status IN ('queued', 'paused')) as "exists!: bool""#,
            self.pretranscoding_id
        )
        .fetch_one(&self.db)
        .await;

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

    /// Records how far a suspended job got, for the UI.
    async fn checkpoint(&self, parked: &Parked) {
        let ms = parked
            .job
            .position()
            .map(|p| p.as_millis() as i64)
            .unwrap_or(0);
        if let Err(err) = sqlx::query!(
            "UPDATE pretranscodings SET transcoded_ms = $1 WHERE id = $2",
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
    }

    async fn persist_progress(&self, parked: &Parked) {
        let position = parked.job.position();
        let ms = position.map(|p| p.as_millis() as i64).unwrap_or(0);
        // Only while running: a tick racing a pause must not flip the UI back.
        let res = sqlx::query!(
            "UPDATE pretranscodings SET transcoded_ms = $1 WHERE id = $2 AND status = 'transcoding'",
            ms,
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
                        download_id: self.output.download_id,
                        transcoded_ms: ms,
                        total_ms: parked.total_ms,
                        status: PretranscodingStatus::Transcoding,
                        waiting_for_pieces: ms == 0,
                    });
            }
            Ok(_) => {}
            Err(err) => {
                tracing::warn!(
                    ?err,
                    self.pretranscoding_id,
                    "Failed to persist transcoded_ms"
                );
            }
        }
    }

    fn emit_progress(&self, ms: i64, total_ms: Option<i64>, status: PretranscodingStatus) {
        self.events
            .transcodings
            .emit_progress(&PretranscodingProgress {
                pretranscoding_id: self.pretranscoding_id,
                download_id: self.output.download_id,
                transcoded_ms: ms,
                total_ms,
                status,
                waiting_for_pieces: false,
            });
    }

    fn emit_status_update(&self, new_status: PretranscodingStatus) {
        self.events.transcodings.emit_status_update(
            &crate::api::transcodings::PretranscodingStatusUpdate {
                pretranscoding_id: self.pretranscoding_id,
                download_id: self.output.download_id,
                new_status,
            },
        );
    }
}
