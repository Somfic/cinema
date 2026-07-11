use std::{path::PathBuf, sync::atomic::Ordering, time::Instant};

use crate::{transcodings::session, utils::supervisor_pool::Acquire};

impl super::Handle {
    /// Start (or reuse) a live HLS playback session for the given file. If a
    /// completed pretranscoded MP4 matches the request, that cached file is
    /// remuxed into HLS (fast, no re-encode); otherwise a full live
    /// transcode is spawned reading from the torrent stream at
    /// [`DownloadPriority::Stream`].
    ///
    /// [`DownloadPriority::Stream`]: crate::downloads::DownloadPriority::Stream
    pub async fn start_playback(
        &self,
        info_hash: &str,
        file_idx: i32,
        audio_index: i32,
        only_audio: bool,
        start_time: f64,
    ) -> crate::app::Result<super::PlaybackSession> {
        // Fast path: an existing pretranscoded MP4 covers this exact request.
        if let Some(cached) = crate::transcodings::types::CompletedPretranscoding::find(
            &self.0.db,
            info_hash,
            file_idx,
            only_audio,
            audio_index,
        )
        .await?
        {
            let cache_path = crate::transcodings::PretranscodingOutputPath::new(
                &self.0.storage,
                cached.download_id,
                only_audio,
                audio_index,
            );

            if tokio::fs::metadata(cache_path.as_ref()).await.is_ok() {
                return self.start_live_local_remux(cache_path, start_time).await;
            }

            // Row says completed but the file's gone. Fail the row and fall
            // through to a fresh live transcode so playback still works.
            tracing::warn!(
                id = cached.id,
                path = %cache_path.display(),
                "Cached pretranscoded MP4 missing on disk; marking failed and falling through to live transcode",
            );
            match sqlx::query!(
                "UPDATE pretranscodings SET status = 'failed', error = 'output file missing' WHERE id = $1",
                cached.id,
            )
            .execute(&self.0.db)
            .await
            {
                Ok(_) => {
                    self.emit_status_update(
                        cached.id,
                        cached.download_id,
                        super::PretranscodingStatus::Failed,
                    );
                }
                Err(err) => {
                    tracing::warn!(
                        id = cached.id,
                        ?err,
                        "Failed to mark pretranscoding as failed; DB out of sync",
                    );
                }
            }
        }

        // Live transcode path. At Stream priority because it *is* a live stream.
        let source = crate::downloads::MediaSource::ensure_and_locate(
            &self.0.downloads_manager,
            &self.0.storage,
            info_hash,
            file_idx,
            crate::downloads::DownloadPriority::Stream,
        )
        .await?;

        self.start_live_transcode(source, audio_index, only_audio, start_time)
            .await
    }

    async fn start_live_transcode(
        &self,
        source: crate::downloads::MediaSource,
        audio_index: i32,
        only_audio: bool,
        start_time: f64,
    ) -> crate::app::Result<super::PlaybackSession> {
        let copy_video =
            only_audio || crate::transcodings::probe::is_browser_safe(source.probe_path()).await;
        let input_display = source.probe_path().display().to_string();

        self.start_live(
            input_display,
            |playlist_path: PathBuf, segment_pattern: PathBuf| async move {
                let command = crate::transcodings::ffmpeg::transcode(
                    &self.0.config,
                    &source,
                    copy_video,
                    start_time,
                    audio_index as usize,
                    &playlist_path,
                    &segment_pattern,
                )
                .await;
                (Some(source), command)
            },
        )
        .await
    }

    async fn start_live_local_remux(
        &self,
        path: impl Into<PathBuf>,
        start_time: f64,
    ) -> crate::app::Result<super::PlaybackSession> {
        let path: PathBuf = path.into();
        let input_display = path.display().to_string();

        self.start_live(
            input_display,
            |playlist_path: PathBuf, segment_pattern: PathBuf| async move {
                let command = crate::transcodings::ffmpeg::local_transcode(
                    start_time,
                    &path,
                    &playlist_path,
                    &segment_pattern,
                );
                (None, command)
            },
        )
        .await
    }

    async fn start_live<
        F: AsyncFnOnce(
            PathBuf,
            PathBuf,
        ) -> (
            Option<crate::downloads::MediaSource>,
            tokio::process::Command,
        ),
    >(
        &self,
        input_display: String,
        create_ffmpeg_command: F,
    ) -> crate::app::Result<super::PlaybackSession> {
        let session_id = session::new_session_id();
        let dir = self.0.storage.join(format!("hls/{session_id}"));
        tokio::fs::create_dir_all(&dir).await?;
        let playlist_path = dir.join("playlist.m3u8");
        let segment_pattern = dir.join("seg%05d.ts");

        let (source, command) = create_ffmpeg_command(playlist_path.clone(), segment_pattern).await;

        self.spawn_and_register_live(
            command,
            source,
            session_id,
            dir,
            playlist_path,
            input_display,
        )
        .await
    }

    async fn spawn_and_register_live(
        &self,
        command: tokio::process::Command,
        source: Option<crate::downloads::MediaSource>,
        session_id: String,
        dir: PathBuf,
        playlist_path: PathBuf,
        input_display: String,
    ) -> crate::app::Result<super::PlaybackSession> {
        let pool_id = self.0.live_pool_id.fetch_sub(1, Ordering::Relaxed);

        // Acquire a capacity slot at Live priority, evicting the oldest
        // background pretranscoding if the pool is full. Live cannot evict
        // Live (both priority 255) - a second concurrent stream when all
        // slots are Live returns NoCapacity, surfaced to the caller.
        let acquire = self
            .0
            .supervisor_pool
            .acquire_evicting(
                pool_id,
                super::TranscodingPriority::Live as u8,
                move |victim| async move { self.evict_pretranscoding_for_stream(victim).await },
            )
            .await?;

        let slot = match acquire {
            Acquire::Acquired(slot) => slot,
            Acquire::AlreadyRunning => {
                // Should be impossible: live pool ids are unique + monotonic.
                let _ = tokio::fs::remove_dir_all(&dir).await;
                return Err(crate::app::CinemaError::Generic(format!(
                    "Live session pool id collision ({pool_id})"
                )));
            }
            Acquire::NoCapacity => {
                let _ = tokio::fs::remove_dir_all(&dir).await;
                return Err(crate::app::CinemaError::Generic(
                    "No capacity for a new live stream. Close another stream and retry".into(),
                ));
            }
        };

        let cancel = slot.cancel_token();
        let cancel_clone = cancel.clone();

        let setup = async {
            let session_id = session_id.clone();

            let session = match session::spawn_live_ffmpeg(
                command,
                source.as_ref(),
                &session_id,
                dir.clone(),
                pool_id,
                input_display,
            )
            .await
            {
                Ok(s) => s,
                Err(err) => {
                    let _ = tokio::fs::remove_dir_all(&dir).await;
                    return Err(err);
                }
            };
            let mut exit_error = session.exit_error.clone();

            // Register the session in the map first so `wait_for_playlist_ready`
            // sees any startup-time ffmpeg errors and `hls_serve` can respond to
            // requests as soon as the first segment lands.
            self.0
                .sessions
                .lock()
                .await
                .insert(session_id.clone(), session);

            if let Err(err) = session::wait_for_playlist_ready(
                &playlist_path,
                &mut exit_error,
                self.0.config.ffmpeg_max_startup_duration,
                self.0.config.ffmpeg_startup_poll_interval,
            )
            .await
            {
                // Startup failed. Drop the session (its Drop kills ffmpeg and
                // removes the dir) and let the slot release when we drop it
                // unspawned.
                self.0.sessions.lock().await.remove(&session_id);
                drop(slot);
                return Err(err);
            }

            // Startup succeeded. Emit the new live-count and spawn the keeper
            // future that holds the slot until stop_live / cleanup_idle_live /
            // shutdown fires the cancel token; on cancel the LiveSession is
            // dropped, which kills ffmpeg and removes its temp dir.
            {
                let map = self.0.sessions.lock().await;
                self.0.events.hls.emit_live_count(&map.len());
            }
            let sessions = self.0.sessions.clone();
            let events = self.0.events.clone();
            let session_id_for_keeper = session_id.clone();
            let playlist_url = format!("/api/hls/{session_id}/playlist.m3u8");
            slot.spawn(async move {
                cancel.cancelled().await;
                let mut map = sessions.lock().await;
                if map.remove(&session_id_for_keeper).is_some() {
                    events.hls.emit_live_count(&map.len());
                }
            });

            Ok(super::PlaybackSession {
                session_id,
                playlist_url,
            })
        };

        tokio::select! {
            biased;
            _ = cancel_clone.cancelled() => {
                {
                    let mut map = self.0.sessions.lock().await;
                    if map.remove(&session_id).is_some() {
                        self.0.events.hls.emit_live_count(&map.len());
                    }
                }

                Err(crate::app::CinemaError::Generic(String::from("Transcoding has been cancelled")))
            }
            res = setup => res
        }
    }

    /// On-evict callback for `acquire_evicting`. Re-queues the victim
    /// pretranscoding in the DB and fires the supervisor's cancel token so
    /// the slot is released. `status = 'queued'` is set BEFORE cancel so the
    /// supervisor reads it as a soft stop: ffmpeg gets SIGINT, the current
    /// segment is finalized with a valid `moov`, `transcoded_ms` is preserved,
    /// and when capacity frees up the pretranscode resumes from checkpoint
    /// (see [`Supervisor::finalize`]).
    ///
    /// [`Supervisor::finalize`]: crate::transcodings::supervisor::Supervisor
    async fn evict_pretranscoding_for_stream(&self, id: i32) -> crate::app::Result<()> {
        tracing::info!(id, "Evicting pretranscoding for live stream");

        // Flip status back to `queued` before cancelling the supervisor.
        // `transcoded_ms` is kept so the resume picks up where we left off.
        let download_id = sqlx::query_scalar!(
            "UPDATE pretranscodings SET status = 'queued', error = NULL WHERE id = $1 AND status = 'transcoding' RETURNING download_id",
            id,
        )
        .fetch_optional(&self.0.db)
        .await?;

        if let Some(download_id) = download_id {
            self.emit_status_update(id, download_id, super::PretranscodingStatus::Queued);
        }

        // Fire the supervisor's cancel token so its finalize runs and the
        // permit is released; the pool's `acquire_evicting` then grabs the
        // freed permit for the incoming live session.
        self.0.supervisor_pool.cancel(id);

        Ok(())
    }

    /// Stop a live session by id. Idempotent for unknown ids.
    pub async fn stop_live(&self, session_id: &str) {
        let pool_id = {
            let mut map = self.0.sessions.lock().await;
            let pool_id = map.remove(session_id).map(|s| s.pool_id);
            if pool_id.is_some() {
                self.0.events.hls.emit_live_count(&map.len());
            }
            pool_id
        };
        if let Some(pool_id) = pool_id {
            self.0.supervisor_pool.cancel(pool_id);
        }
    }

    /// Current number of live HLS sessions. Reads the in-memory session map.
    pub async fn live_session_count(&self) -> usize {
        self.0.sessions.lock().await.len()
    }

    /// Update a live session's last-access timestamp. Called on every
    /// segment request so the idle reaper only culls sessions the client
    /// has stopped consuming.
    pub async fn touch_live(&self, session_id: &str) {
        if let Some(session) = self.0.sessions.lock().await.get_mut(session_id) {
            session.last_access = Instant::now();
        }
    }

    /// Where the ffmpeg process is writing this session's HLS segments.
    /// Consumed by `raw::hls_serve` to serve the playlist + segment files.
    pub async fn live_session_dir(&self, session_id: &str) -> Option<PathBuf> {
        self.0
            .sessions
            .lock()
            .await
            .get(session_id)
            .map(|s| s.dir.clone())
    }

    /// If the ffmpeg process for this session exited with an error, return
    /// its tail of stderr. Used by `raw::hls_serve` to surface a useful
    /// message when a segment 404s because ffmpeg died.
    pub async fn live_session_error(&self, session_id: &str) -> Option<String> {
        let map = self.0.sessions.lock().await;
        let session = map.get(session_id)?;
        session.exit_error.borrow().clone()
    }

    /// Cull sessions that haven't been touched in `max_idle_secs`. Returns
    /// how many were stopped.
    pub async fn cleanup_idle_live(&self, max_idle: std::time::Duration) -> usize {
        let stale: Vec<i32> = {
            let mut map = self.0.sessions.lock().await;
            let now = Instant::now();
            let stale: Vec<i32> = map
                .extract_if(|_, session| now.duration_since(session.last_access) > max_idle)
                .map(|(_, session)| session.pool_id)
                .collect();
            if !stale.is_empty() {
                self.0.events.hls.emit_live_count(&map.len());
            }
            stale
        };
        let count = stale.len();
        for pool_id in stale {
            self.0.supervisor_pool.cancel(pool_id);
        }
        count
    }

    /// Stop every live session. Used at shutdown and from the "kill all"
    /// action on the Downloads popover.
    pub async fn stop_all_live(&self) {
        let pool_ids: Vec<i32> = {
            let mut map = self.0.sessions.lock().await;
            let pool_ids: Vec<i32> = map.drain().map(|(_, session)| session.pool_id).collect();
            if !pool_ids.is_empty() {
                self.0.events.hls.emit_live_count(&map.len());
            }
            pool_ids
        };

        for pool_id in pool_ids {
            self.0.supervisor_pool.cancel(pool_id);
        }
    }
}
