use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use crate::api::watch::TranscodingOption;
use crate::utils::supervisor_pool::Acquire;

/// What the playing client can decode, in short codec names (`h264`,
/// `hevc`, `aac`, `eac3`, ...) and container names (`mp4`, `webm`, ...).
#[draad::ty]
pub struct ClientCapabilities {
    pub video_codecs: Vec<String>,
    pub audio_codecs: Vec<String>,
    pub containers: Vec<String>,
    pub max_height: Option<u32>,
}

impl From<ClientCapabilities> for media::ClientCaps {
    fn from(c: ClientCapabilities) -> Self {
        Self {
            video_codecs: c.video_codecs,
            audio_codecs: c.audio_codecs,
            containers: c.containers,
            max_height: c.max_height,
        }
    }
}

#[draad::ty]
#[derive(PartialEq)]
pub enum PlaybackKind {
    /// `url` is the file itself, range-served.
    Direct,
    /// `url` is an HLS master playlist.
    Hls,
}

/// What happens to one stream on its way to the client.
#[draad::ty]
#[derive(PartialEq)]
pub enum StreamAction {
    /// Passed through untouched.
    Copy,
    /// Re-encoded to something the client decodes.
    Transcode,
    /// There is no such stream.
    None,
}

#[draad::ty]
pub struct Playback {
    pub kind: PlaybackKind,
    pub url: String,
    /// Set for HLS; stop it when done.
    pub session_id: Option<String>,
    pub video: StreamAction,
    pub audio: StreamAction,
    /// Seconds, when known.
    pub duration: Option<f64>,
}

pub(super) struct LiveSession {
    pub(super) session: Arc<media::hls::Session>,
    pub(super) last_access: Instant,
    /// The [`SupervisorPool`] key reserving this session's capacity slot.
    /// Only sessions that re-encode video take one, and only while one is
    /// free: live streams are never refused. Negative, so it can't collide
    /// with pretranscoding row ids.
    ///
    /// [`SupervisorPool`]: crate::utils::supervisor_pool::SupervisorPool
    pub(super) pool_id: Option<i32>,
}

/// Random 16-char hex session id.
fn new_session_id() -> String {
    use rand::Rng;

    let mut bytes = [0u8; 8];
    rand::rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

impl super::Handle {
    /// Works out the least work that gets this file playing on the client,
    /// and starts it. A completed pretranscode is preferred over the
    /// original. Then, depending on what the client decodes: the file plays
    /// as is, or it's packaged as HLS with each stream copied when possible
    /// and re-encoded only when not. `mode` can force re-encoding.
    pub async fn start_playback(
        &self,
        info_hash: &str,
        file_idx: i32,
        audio_index: i32,
        caps: ClientCapabilities,
        mode: TranscodingOption,
    ) -> crate::app::Result<Playback> {
        let audio_index = audio_index.max(0) as usize;

        // A finished pretranscode already holds just the chosen audio track,
        // in formats every client plays.
        let cached = self
            .cached_pretranscode(info_hash, file_idx, audio_index as i32)
            .await?;
        let (input, key, direct_url, audio_index, source) = match cached {
            Some(path) => (
                media::Input::File(path.to_path_buf()),
                path.display().to_string(),
                format!("/api/files/{}", path.storage_relative()),
                0,
                None,
            ),
            None => {
                let source = crate::downloads::MediaSource::ensure_and_locate(
                    &self.0.downloads_manager,
                    &self.0.storage,
                    info_hash,
                    file_idx,
                    crate::downloads::DownloadPriority::Stream,
                )
                .await?;
                (
                    source.media_input().await?,
                    format!("{info_hash}/{file_idx}"),
                    format!("/api/stream/{info_hash}/{file_idx}"),
                    audio_index,
                    Some(source),
                )
            }
        };

        let info = self.media_info(&key, &input).await?;
        let request = media::PlanRequest {
            audio_index,
            force_video_transcode: matches!(mode, TranscodingOption::Enabled),
            force_audio_transcode: matches!(
                mode,
                TranscodingOption::Enabled | TranscodingOption::OnlyAudio
            ),
        };
        let duration = info.duration.map(|d| d.as_secs_f64());
        let plan = media::plan(&info, &caps.into(), &request);
        tracing::info!(info_hash, file_idx, audio_index, ?plan, "Playback plan");

        let (video, audio) = match plan {
            media::Plan::Direct => {
                return Ok(Playback {
                    kind: PlaybackKind::Direct,
                    url: direct_url,
                    session_id: None,
                    video: StreamAction::Copy,
                    audio: if info.audio.is_empty() {
                        StreamAction::None
                    } else {
                        StreamAction::Copy
                    },
                    duration,
                });
            }
            media::Plan::Hls { video, audio } => (video, audio),
        };

        // Point the swarm at wherever a run starts, so a seek's pieces are on
        // their way while the pipeline spins up.
        let on_seek: Option<Arc<dyn Fn(Duration) + Send + Sync>> = match (&source, duration) {
            (Some(crate::downloads::MediaSource::Engine { .. }), Some(total)) => {
                let downloads = self.0.downloads_manager.clone();
                let info_hash = info_hash.to_string();
                let runtime = tokio::runtime::Handle::current();
                Some(Arc::new(move |at: Duration| {
                    let downloads = downloads.clone();
                    let info_hash = info_hash.clone();
                    runtime.spawn(async move {
                        downloads
                            .prioritize_position(&info_hash, file_idx, at.as_secs_f64(), total)
                            .await;
                    });
                }))
            }
            _ => None,
        };

        let session_id = new_session_id();
        let session = media::hls::Session::new(media::hls::SessionOptions {
            input,
            info,
            video,
            audio,
            audio_index,
            encoder: self.0.config.encoder(),
            dir: self.0.storage.hls_dir().join(&session_id),
            on_seek,
        })
        .await?;

        // Re-encoding video is the expensive part, so it pushes background
        // pretranscodes out of the way. It never waits for other live
        // streams, though.
        let pool_id = if video == media::VideoAction::Transcode {
            let pool_id = self.0.live_pool_id.fetch_sub(1, Ordering::Relaxed);
            self.reserve_live_slot(pool_id, session_id.clone())
                .await?
                .then_some(pool_id)
        } else {
            None
        };

        {
            let mut sessions = self.0.sessions.lock().unwrap();
            sessions.insert(
                session_id.clone(),
                LiveSession {
                    session: Arc::new(session),
                    last_access: Instant::now(),
                    pool_id,
                },
            );
            self.0.events.hls.emit_live_count(&sessions.len());
        }

        let action = |transcode: bool| {
            if transcode {
                StreamAction::Transcode
            } else {
                StreamAction::Copy
            }
        };
        Ok(Playback {
            kind: PlaybackKind::Hls,
            url: format!("/api/hls/{session_id}/master.m3u8"),
            session_id: Some(session_id),
            video: action(video == media::VideoAction::Transcode),
            audio: match audio {
                media::AudioAction::None => StreamAction::None,
                a => action(a == media::AudioAction::Transcode),
            },
            duration,
        })
    }

    /// A completed pretranscode of this file and audio track whose output is
    /// on disk. Rows whose file has gone missing are failed.
    async fn cached_pretranscode(
        &self,
        info_hash: &str,
        file_idx: i32,
        audio_index: i32,
    ) -> crate::app::Result<Option<crate::transcodings::PretranscodingOutputPath>> {
        // A full transcode plays anywhere; an audio-only one keeps the
        // original video.
        for only_audio in [false, true] {
            let Some(cached) = crate::transcodings::types::CompletedPretranscoding::find(
                &self.0.db,
                info_hash,
                file_idx,
                only_audio,
                audio_index,
            )
            .await?
            else {
                continue;
            };
            let path = crate::transcodings::PretranscodingOutputPath::new(
                &self.0.storage,
                cached.download_id,
                only_audio,
                audio_index,
            );
            if tokio::fs::metadata(path.as_ref()).await.is_ok() {
                return Ok(Some(path));
            }

            tracing::warn!(
                id = cached.id,
                path = %path.display(),
                "Cached pretranscoded MP4 missing on disk; marking failed",
            );
            match sqlx::query!(
                "UPDATE pretranscodings SET status = 'failed', error = 'output file missing' WHERE id = $1",
                cached.id,
            )
            .execute(&self.0.db)
            .await
            {
                Ok(_) => self.emit_status_update(
                    cached.id,
                    cached.download_id,
                    super::PretranscodingStatus::Failed,
                ),
                Err(err) => {
                    tracing::warn!(id = cached.id, ?err, "Failed to mark pretranscoding as failed");
                }
            }
        }
        Ok(None)
    }

    /// Holds a Live capacity slot for `session_id` until the session stops,
    /// evicting a background pretranscode if that's what it takes. False when
    /// every slot is already held by a live stream: the session then runs
    /// without one rather than being refused.
    async fn reserve_live_slot(
        &self,
        pool_id: i32,
        session_id: String,
    ) -> crate::app::Result<bool> {
        let slot = match self.acquire_live_slot(pool_id).await? {
            Acquire::Acquired(slot) => slot,
            Acquire::NoCapacity => return Ok(false),
            Acquire::AlreadyRunning => {
                return Err(crate::app::CinemaError::Generic(format!(
                    "Live session pool id collision ({pool_id})"
                )));
            }
        };

        // The keeper holds the slot until stop_live / cleanup_idle_live /
        // shutdown fires its token, then drops the session.
        let cancel = slot.cancel_token();
        let handle = self.clone();
        slot.spawn(async move {
            cancel.cancelled().await;
            handle.remove_session(&session_id);
        });
        Ok(true)
    }

    async fn acquire_live_slot(&self, pool_id: i32) -> crate::app::Result<Acquire> {
        self.0
            .supervisor_pool
            .acquire_evicting(
                pool_id,
                super::TranscodingPriority::Live as u8,
                move |victim| async move { self.evict_pretranscoding_for_stream(victim).await },
            )
            .await
    }

    /// On-evict callback for `acquire_evicting`. Re-queues the victim
    /// pretranscoding in the DB and fires the supervisor's cancel token so
    /// the slot is released. `status = 'queued'` is set BEFORE cancel so the
    /// supervisor reads it as a soft stop: the job is suspended and parked,
    /// and resumes when capacity frees up.
    async fn evict_pretranscoding_for_stream(&self, id: i32) -> crate::app::Result<()> {
        tracing::info!(id, "Evicting pretranscoding for live stream");

        let download_id = sqlx::query_scalar!(
            "UPDATE pretranscodings SET status = 'queued', error = NULL WHERE id = $1 AND status = 'transcoding' RETURNING download_id",
            id,
        )
        .fetch_optional(&self.0.db)
        .await?;

        if let Some(download_id) = download_id {
            self.emit_status_update(id, download_id, super::PretranscodingStatus::Queued);
        }

        self.0.supervisor_pool.cancel(id);

        Ok(())
    }

    /// Serves a playlist or segment of a live session, by its file name in
    /// the playlist.
    pub async fn serve_live(
        &self,
        session_id: &str,
        file: &str,
    ) -> crate::app::Result<(bytes::Bytes, &'static str)> {
        let session = {
            let mut sessions = self.0.sessions.lock().unwrap();
            let live = sessions
                .get_mut(session_id)
                .ok_or_else(|| crate::app::CinemaError::NotFound("HLS session not found".into()))?;
            live.last_access = Instant::now();
            live.session.clone()
        };
        Ok(session.serve(file).await?)
    }

    fn remove_session(&self, session_id: &str) -> Option<LiveSession> {
        let mut sessions = self.0.sessions.lock().unwrap();
        let removed = sessions.remove(session_id);
        if removed.is_some() {
            self.0.events.hls.emit_live_count(&sessions.len());
        }
        removed
    }

    /// Stop a live session by id. Idempotent for unknown ids.
    pub async fn stop_live(&self, session_id: &str) {
        if let Some(pool_id) = self.remove_session(session_id).and_then(|s| s.pool_id) {
            self.0.supervisor_pool.cancel(pool_id);
        }
    }

    /// Current number of live HLS sessions.
    pub async fn live_session_count(&self) -> usize {
        self.0.sessions.lock().unwrap().len()
    }

    /// Stops sessions that haven't been touched in `max_idle`. Returns how
    /// many were stopped.
    pub async fn cleanup_idle_live(&self, max_idle: Duration) -> usize {
        let stale: Vec<LiveSession> = {
            let mut sessions = self.0.sessions.lock().unwrap();
            let now = Instant::now();
            let stale: Vec<LiveSession> = sessions
                .extract_if(|_, s| now.duration_since(s.last_access) > max_idle)
                .map(|(_, s)| s)
                .collect();
            if !stale.is_empty() {
                self.0.events.hls.emit_live_count(&sessions.len());
            }
            stale
        };
        let count = stale.len();
        for pool_id in stale.into_iter().filter_map(|s| s.pool_id) {
            self.0.supervisor_pool.cancel(pool_id);
        }
        count
    }

    /// Stop every live session. Used at shutdown and from the "kill all"
    /// action on the Downloads popover.
    pub async fn stop_all_live(&self) {
        let pool_ids: Vec<i32> = {
            let mut sessions = self.0.sessions.lock().unwrap();
            let pool_ids = sessions.drain().filter_map(|(_, s)| s.pool_id).collect();
            self.0.events.hls.emit_live_count(&0);
            pool_ids
        };
        for pool_id in pool_ids {
            self.0.supervisor_pool.cancel(pool_id);
        }
    }
}
