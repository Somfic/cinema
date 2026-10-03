use crate::utils::supervisor_pool::Acquire;

impl super::Handle {
    /// Queue a pretranscoding for the given download + audio track + mode.
    /// Idempotent - a duplicate returns the existing row id.
    pub async fn enqueue(
        &self,
        download_id: i32,
        only_audio: bool,
        audio_index: i32,
    ) -> crate::app::Result<i32> {
        let mut tx = self.0.db.begin().await?;

        // Verify the download exists (fail early rather than dangling FK).
        let exists = sqlx::query_scalar!(
            "SELECT id FROM downloads WHERE id = $1 FOR UPDATE",
            download_id
        )
        .fetch_optional(&mut *tx)
        .await?;

        if exists.is_none() {
            return Err(crate::app::CinemaError::NotFound(format!(
                "Download {download_id} not found"
            )));
        }

        // If a queued/transcoding/completed row exists, return it.
        // Otherwise create a new queued row (upserting over any prior terminal-state row for the same key).
        let existing = sqlx::query_scalar!(
            r#"
                SELECT id
                FROM pretranscodings
                WHERE download_id = $1
                    AND only_audio = $2
                    AND audio_index = $3
                    AND status in ('queued', 'transcoding', 'paused', 'completed')
            "#,
            download_id,
            only_audio,
            audio_index,
        )
        .fetch_optional(&mut *tx)
        .await?;

        if let Some(id) = existing {
            return Ok(id);
        }

        let id = sqlx::query_scalar!(
            r#"
                INSERT INTO pretranscodings (download_id, only_audio, audio_index)
                VALUES ($1, $2, $3)
                    ON CONFLICT (download_id, only_audio, audio_index) DO UPDATE SET
                        status = DEFAULT,
                        error = DEFAULT,
                        transcoded_ms = DEFAULT,
                        total_ms = DEFAULT,
                        completed_at = DEFAULT,
                        created_at = DEFAULT
                RETURNING id
            "#,
            download_id,
            only_audio,
            audio_index,
        )
        .fetch_one(&mut *tx)
        .await?;

        tx.commit().await?;

        self.emit_status_update(id, download_id, super::PretranscodingStatus::Queued);
        self.0.supervisor_pool.nudge().await;

        Ok(id)
    }

    /// Pause a running (or queued) pretranscoding. A running job is
    /// suspended in place and parked; [`resume`](Self::resume) continues it.
    pub async fn pause(&self, id: i32) -> crate::app::Result<()> {
        // Set the target status BEFORE firing the cancel token so the
        // supervisor reads `paused` and suspends rather than discards.
        let download_id = sqlx::query_scalar!(
            "UPDATE pretranscodings SET status = 'paused' WHERE id = $1 AND status IN ('queued', 'transcoding') RETURNING download_id",
            id,
        )
        .fetch_optional(&self.0.db)
        .await?;

        if let Some(download_id) = download_id {
            if self.0.supervisor_pool.cancel(id) {
                tracing::info!(id, "Pausing the pretranscoding");
            }

            self.emit_status_update(id, download_id, super::PretranscodingStatus::Paused);
        }

        Ok(())
    }

    /// Resume a paused pretranscoding: flip back to `queued` and nudge the
    /// pool. `refresh()` picks it up and resumes the parked job, or starts
    /// over if it didn't survive a restart.
    pub async fn resume(&self, id: i32) -> crate::app::Result<()> {
        let download_id = sqlx::query_scalar!(
            "UPDATE pretranscodings SET status = 'queued', error = NULL WHERE id = $1 AND status = 'paused' RETURNING download_id",
            id,
        )
        .fetch_optional(&self.0.db)
        .await?;

        if let Some(download_id) = download_id {
            self.emit_status_update(id, download_id, super::PretranscodingStatus::Queued);
            self.0.supervisor_pool.nudge().await;
        }

        Ok(())
    }

    /// Cancel a running/queued/paused pretranscoding. Deletes the partial
    /// output; leaves the row in `cancelled` state so the user can see what
    /// happened.
    pub async fn cancel(&self, id: i32) -> crate::app::Result<()> {
        let row = sqlx::query!(
            "SELECT download_id, only_audio, audio_index FROM pretranscodings WHERE id = $1",
            id
        )
        .fetch_optional(&self.0.db)
        .await?;

        // Set `cancelled` first so a running supervisor sees a non-soft target
        // status when the cancel token fires and discards its output itself.
        let res = sqlx::query!(
            "UPDATE pretranscodings SET status = 'cancelled' WHERE id = $1 AND status NOT IN ('completed', 'failed')",
            id,
        )
        .execute(&self.0.db)
        .await?;

        let was_running = self.0.supervisor_pool.cancel(id);
        if was_running {
            tracing::info!(id, "Cancelling the pretranscoding");
        }

        if let Some(row) = &row {
            let path = crate::transcodings::PretranscodingOutputPath::new(
                &self.0.storage,
                row.download_id,
                row.only_audio,
                row.audio_index,
            );
            if !was_running {
                // No supervisor to clean up after itself. Dropping a parked
                // job removes its partial output.
                self.0.parked.lock().unwrap().remove(&id);
                path.remove().await;
            }
        }

        if let Some(row) = row
            && res.rows_affected() > 0
        {
            self.emit_status_update(id, row.download_id, super::PretranscodingStatus::Cancelled);
        }

        Ok(())
    }

    /// Cancel if running, delete the row, and remove any cached files.
    pub async fn remove(&self, id: i32) -> crate::app::Result<()> {
        let mut tx = self.0.db.begin().await?;

        // Lock the parent download so a concurrent enqueue for the same
        // download can't race us between the row lookup and the delete.
        let row = sqlx::query!(
            r#"
                SELECT pt.download_id, pt.only_audio, pt.audio_index
                FROM pretranscodings pt
                JOIN downloads d ON d.id = pt.download_id
                WHERE pt.id = $1
                FOR UPDATE OF d
            "#,
            id
        )
        .fetch_optional(&mut *tx)
        .await?;

        // Cancel inside the tx: the row lock keeps refresh() from spawning a
        // fresh supervisor for this id until we commit.
        if self.0.supervisor_pool.cancel(id) {
            tracing::info!(id, "Removing the pretranscoding");
        }

        sqlx::query!("DELETE FROM pretranscodings WHERE id = $1", id)
            .execute(&mut *tx)
            .await?;

        tx.commit().await?;

        self.0.parked.lock().unwrap().remove(&id);
        if let Some(row) = row {
            crate::transcodings::PretranscodingOutputPath::new(
                &self.0.storage,
                row.download_id,
                row.only_audio,
                row.audio_index,
            )
            .remove()
            .await;

            self.0.events.transcodings.emit_removed(
                &crate::api::transcodings::PretranscodingRemoved {
                    pretranscoding_id: id,
                    download_id: row.download_id,
                },
            );
        }

        Ok(())
    }

    /// Cancel every supervisor for a download and delete its cached files.
    pub async fn remove_all_for_download(&self, download_id: i32) -> crate::app::Result<()> {
        let mut tx = self.0.db.begin().await?;

        // Lock the parent download so enqueue can't slip in a new row while
        // we're tearing down. If the download is already gone the lock is a
        // no-op and the delete below just returns an empty set.
        sqlx::query_scalar!(
            "SELECT id FROM downloads WHERE id = $1 FOR UPDATE",
            download_id,
        )
        .fetch_optional(&mut *tx)
        .await?;

        let rows = sqlx::query!(
            "DELETE FROM pretranscodings WHERE download_id = $1 RETURNING id, only_audio, audio_index",
            download_id,
        )
        .fetch_all(&mut *tx)
        .await?;

        self.0
            .supervisor_pool
            .cancel_all(rows.iter().map(|row| row.id));

        tx.commit().await?;

        for row in &rows {
            self.0.parked.lock().unwrap().remove(&row.id);
            crate::transcodings::PretranscodingOutputPath::new(
                &self.0.storage,
                download_id,
                row.only_audio,
                row.audio_index,
            )
            .remove()
            .await;

            self.0.events.transcodings.emit_removed(
                &crate::api::transcodings::PretranscodingRemoved {
                    pretranscoding_id: row.id,
                    download_id,
                },
            );
        }

        Ok(())
    }

    pub(super) async fn start(&self, id: i32) -> crate::app::Result<()> {
        if self.0.supervisor_pool.is_running(id) {
            return Ok(());
        }

        let row = sqlx::query!(
            r#"
                SELECT
                    pt.download_id,
                    pt.only_audio,
                    pt.audio_index,
                    pt.status as "status: super::PretranscodingStatus",
                    d.info_hash,
                    d.file_idx
                FROM pretranscodings pt
                JOIN downloads d ON d.id = pt.download_id
                WHERE pt.id = $1
            "#,
            id,
        )
        .fetch_optional(&self.0.db)
        .await
        .map_err(crate::app::CinemaError::DatabaseError)?
        .ok_or_else(|| {
            crate::app::CinemaError::NotFound(format!("Pretranscoding {id} not found"))
        })?;

        if row.status != super::PretranscodingStatus::Queued {
            return Ok(());
        }

        let acquire = self
            .0
            .supervisor_pool
            .acquire(id, super::TranscodingPriority::Pretranscoding as u8)
            .await;
        let slot = match acquire {
            Acquire::Acquired(slot) => slot,
            Acquire::AlreadyRunning | Acquire::NoCapacity => return Ok(()),
        };

        let cancel = slot.cancel_token();
        let cancel_clone = cancel.clone();

        let start = async {
            let output = crate::transcodings::PretranscodingOutputPath::new(
                &self.0.storage,
                row.download_id,
                row.only_audio,
                row.audio_index,
            );
            let parked = self.0.parked.lock().unwrap().remove(&id);
            let start = match parked {
                Some(parked) => crate::transcodings::supervisor::Start::Parked(parked),
                None => match crate::downloads::MediaSource::ensure_and_locate(
                    &self.0.downloads_manager,
                    &self.0.storage,
                    &row.info_hash,
                    row.file_idx,
                    crate::downloads::DownloadPriority::Background,
                )
                .await
                {
                    Ok(source) => crate::transcodings::supervisor::Start::Fresh {
                        source,
                        only_audio: row.only_audio,
                        audio_index: row.audio_index,
                    },
                    Err(err) => {
                        self.fail(
                            id,
                            row.download_id,
                            &format!("MediaSource not found: {err:?}"),
                        )
                        .await;
                        return Err(err);
                    }
                },
            };

            let supervisor = crate::transcodings::supervisor::Supervisor {
                pretranscoding_id: id,
                output,
                db: self.0.db.clone(),
                events: self.0.events.clone(),
                config: self.0.config.clone(),
                parked: self.0.parked.clone(),
                cancel,
            };

            slot.spawn(async move {
                supervisor.run(start).await;
            });

            Ok(())
        };

        tokio::select! {
            biased;
            _ = cancel_clone.cancelled() => {
                Ok(())
            }
            res = start => res
        }
    }

    async fn fail(&self, id: i32, download_id: i32, error: &str) {
        tracing::warn!(id, "Pretranscode failed: {error}");
        if let Err(err) = sqlx::query!(
            "UPDATE pretranscodings SET status = 'failed', error = $1 WHERE id = $2 AND status NOT IN ('cancelled')",
            error,
            id,
        )
        .execute(&self.0.db)
        .await
        {
            tracing::error!(?err, id, "Failed to record failure");
        }
        self.emit_status_update(id, download_id, super::PretranscodingStatus::Failed);
    }
}
