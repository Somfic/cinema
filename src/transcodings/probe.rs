//! ffprobe-based checks used both by pretranscoding (skip re-encode when the
//! source is already browser-safe) and by the live HLS path (same reasoning).

/// Browser-safe video codecs that can be copied directly into HLS.
const BROWSER_SAFE_VIDEO: &[&str] = &["h264", "avc", "avc1"];

pub async fn is_browser_safe(path: &std::path::Path) -> bool {
    let video_codec = probe_video_codec(path).await.unwrap_or_default();
    BROWSER_SAFE_VIDEO.iter().any(|c| video_codec.contains(c))
}

/// Probe the video codec of a file using ffprobe. `input` accepts either an
/// on-disk path or an ffmpeg-style URL/pipe descriptor.
async fn probe_video_codec(path: &std::path::Path) -> Option<String> {
    let output = tokio::process::Command::new("ffprobe")
        .args([
            "-v",
            "quiet",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=codec_name",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let codec = String::from_utf8_lossy(&output.stdout)
        .trim()
        .to_lowercase();
    if codec.is_empty() { None } else { Some(codec) }
}
