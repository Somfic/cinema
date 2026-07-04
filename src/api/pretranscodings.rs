use crate::app::{AppContext, Error};
use crate::pretranscodings::PretranscodingProgress;
use crate::pretranscodings::types::{Pretranscoding, PretranscodingStatus};

#[draad::ty]
pub struct EnqueuePretranscoding {
    pub download_id: i32,
    pub only_audio: bool,
    pub audio_index: i32,
}

#[draad::ty]
pub struct PretranscodingStatusUpdate {
    pub pretranscoding_id: i32,
    pub download_id: i32,
    pub new_status: PretranscodingStatus,
}

#[draad::ty]
pub struct PretranscodingRemoved {
    pub pretranscoding_id: i32,
    pub download_id: i32,
}

#[draad::api(namespace = "pretranscodings")]
pub trait PretranscodingsApi {
    /// Lists every pretranscoding row across all downloads, newest first
    #[get]
    async fn list(&self) -> Result<Vec<Pretranscoding>, Error>;

    /// Queue a pretranscoding job. Idempotent: an existing queued/running/completed
    /// row for the same (download, only_audio, audio_index) is returned unchanged.
    async fn enqueue(&self, request: EnqueuePretranscoding) -> Result<i32, Error>;

    /// Cancel a running or queued pretranscoding. The partial output file is
    /// removed; the row is kept in `cancelled` state so the user can see it.
    async fn cancel(&self, id: i32) -> Result<(), Error>;

    /// Delete the pretranscoding row and remove its cached MP4 from disk.
    #[post]
    async fn remove(&self, id: i32) -> Result<(), Error>;
}

#[draad::api]
impl PretranscodingsApi for AppContext {
    async fn list(&self) -> Result<Vec<Pretranscoding>, Error> {
        Pretranscoding::find_all(&self.db).await
    }

    async fn enqueue(&self, request: EnqueuePretranscoding) -> Result<i32, Error> {
        self.pretranscodings
            .enqueue(request.download_id, request.only_audio, request.audio_index)
            .await
    }

    async fn cancel(&self, id: i32) -> Result<(), Error> {
        self.pretranscodings.cancel(id).await
    }

    async fn remove(&self, id: i32) -> Result<(), Error> {
        self.pretranscodings.remove(id).await
    }
}

#[draad::events(namespace = "pretranscodings")]
pub trait PretranscodingsEvents {
    /// Per-pretranscoding tick with the current transcoded position.
    fn progress(payload: PretranscodingProgress);

    fn status_update(payload: PretranscodingStatusUpdate);

    fn removed(payload: PretranscodingRemoved);
}
