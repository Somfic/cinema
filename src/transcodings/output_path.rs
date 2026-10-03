use std::path::PathBuf;

#[derive(Debug)]
pub struct PretranscodingOutputPath {
    pub(super) output_path: PathBuf,
    pub(super) download_id: i32,
    pub(super) only_audio: bool,
    pub(super) audio_index: i32,
}

impl From<PretranscodingOutputPath> for PathBuf {
    fn from(p: PretranscodingOutputPath) -> PathBuf {
        p.output_path
    }
}

impl std::ops::Deref for PretranscodingOutputPath {
    type Target = PathBuf;

    fn deref(&self) -> &Self::Target {
        &self.output_path
    }
}

impl AsRef<std::path::Path> for PretranscodingOutputPath {
    fn as_ref(&self) -> &std::path::Path {
        &self.output_path
    }
}

impl PretranscodingOutputPath {
    /// Where a cached MP4 lives on disk. Encodes `download_id`, mode, and audio
    /// track into the filename so all three permutations can coexist for the
    /// same download.
    pub fn new(
        storage: &crate::app::Storage,
        download_id: i32,
        only_audio: bool,
        audio_index: i32,
    ) -> Self {
        let mode = if only_audio { "audio" } else { "full" };
        let output_path = storage.join(format!(
            "pretranscoded/{download_id}_{mode}_{audio_index}.mp4"
        ));

        Self {
            output_path,
            download_id,
            only_audio,
            audio_index,
        }
    }

    /// The in-progress output, renamed to the final path on completion.
    pub fn partial(&self) -> PathBuf {
        self.output_path.with_extension("mp4.part")
    }

    /// Path relative to storage, for the `/api/files` route.
    pub fn storage_relative(&self) -> String {
        let mode = if self.only_audio { "audio" } else { "full" };
        format!(
            "pretranscoded/{}_{mode}_{}.mp4",
            self.download_id, self.audio_index
        )
    }

    /// Removes the output and any partial leftovers.
    pub async fn remove(&self) {
        for path in [
            self.output_path.clone(),
            self.partial(),
            self.output_path.with_extension("mp4.moov"),
        ] {
            if let Err(err) = tokio::fs::remove_file(&path).await
                && err.kind() != std::io::ErrorKind::NotFound
            {
                tracing::warn!(?err, ?path, "Could not remove pretranscoding output");
            }
        }
    }

    /// Total on-disk size: the finished MP4 or, while running, the partial
    /// output. Missing files contribute 0.
    pub async fn disk_bytes(&self) -> u64 {
        let mut total: u64 = 0;
        for path in [self.output_path.clone(), self.partial()] {
            if let Ok(meta) = tokio::fs::metadata(&path).await {
                total = total.saturating_add(meta.len());
            }
        }
        total
    }
}
