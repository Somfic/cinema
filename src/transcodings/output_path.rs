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
}
