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

    /// Path for segment N. Pause/resume writes segments 0, 1, 2, … and the
    /// final `.mp4` is produced by concat-copying them at completion.
    pub fn segment(&self, n: u32) -> PathBuf {
        self.output_path.with_extension(format!("mp4.part.{n}"))
    }

    /// Existing segments on disk, sorted by index. Returns `(index, path)` pairs.
    /// Non-numeric or malformed `.part.*` files are silently skipped.
    pub async fn existing_segments(&self) -> Vec<(u32, PathBuf)> {
        let Some(parent) = self.output_path.parent() else {
            return Vec::new();
        };
        let Some(stem) = self.output_path.file_name().and_then(|s| s.to_str()) else {
            return Vec::new();
        };
        let prefix = format!("{stem}.part.");

        let mut entries = match tokio::fs::read_dir(parent).await {
            Ok(rd) => rd,
            Err(_) => return Vec::new(),
        };

        let mut out = Vec::new();
        while let Ok(Some(entry)) = entries.next_entry().await {
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            let Some(suffix) = name.strip_prefix(&prefix) else {
                continue;
            };
            let Ok(idx) = suffix.parse::<u32>() else {
                continue;
            };
            out.push((idx, entry.path()));
        }
        out.sort_by_key(|(n, _)| *n);
        out
    }

    /// Remove every `.mp4.part.*` segment for this row. Used on hard cancel /
    /// remove / boot recovery of interrupted transcodings.
    pub async fn remove_all_segments(&self) {
        for (_, path) in self.existing_segments().await {
            if let Err(err) = tokio::fs::remove_file(&path).await {
                tracing::warn!(?err, ?path, "Could not remove pretranscoding segment");
            }
        }
    }
}
