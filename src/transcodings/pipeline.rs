/// Encoder keyframe interval (in frames) for transcoded video. ~2s at 24-30fps,
/// matching the 2s HLS segment length so the muxer always has a cut point.
const GOP_FRAMES: u32 = 48;

/// The video half of an ffmpeg invocation, split by where each arg must go.
pub struct VideoPipeline {
    /// Decode-side args that must precede `-i` (e.g. `-hwaccel`).
    pub pre_input: Vec<String>,
    /// Output-side filter args (e.g. `-vf scale_cuda=...`).
    pub filter: Vec<String>,
    /// Encoder selection + tuning (`-c:v ...`).
    pub encode: Vec<String>,
}

impl VideoPipeline {
    /// Decide how to handle the video stream. `copy_video` streams it through
    /// untouched; otherwise we prefer a fully GPU-resident NVENC pipeline (decode →
    /// scale_cuda → encode, no CPU frame copy) and fall back to software libx264.
    ///
    /// The NVENC path matters for load: with `-hwaccel auto` ffmpeg decodes on the
    /// GPU but downloads every frame to do the 10-bit→8-bit `-pix_fmt` conversion on
    /// the CPU (~4 cores for 4K). Keeping frames on the GPU via `scale_cuda` drops
    /// that to a fraction.
    ///
    /// `low_latency` chooses between HLS-style tuning (fast segment cadence, uses
    /// `-tune ll` on NVENC) and background pretranscode tuning (quality preset,
    /// no low-latency tricks).
    pub async fn new(config: &crate::Config, copy_video: bool, low_latency: bool) -> VideoPipeline {
        if copy_video {
            return VideoPipeline {
                pre_input: Vec::new(),
                filter: Vec::new(),
                encode: vec!["-c:v".into(), "copy".into()],
            };
        }

        let want_nvenc = matches!(config.ffmpeg_hwaccel.as_str(), "auto" | "cuda" | "nvenc");
        if want_nvenc && nvenc_available().await {
            let mut encode: Vec<String> = vec![
                "-c:v".into(),
                "h264_nvenc".into(),
                "-preset".into(),
                if low_latency {
                    "p4".into()
                } else {
                    "p6".into()
                },
            ];
            if low_latency {
                encode.extend_from_slice(&[
                    "-tune".into(),
                    "ll".into(),
                    // Fixed keyframe interval (~2s at 24-30fps). Without it, nvenc
                    // with `-tune ll` emits only the opening keyframe and *ignores*
                    // `-force_key_frames`, so the HLS muxer finds no split points,
                    // buffers everything into segment 0, and only flushes at EOF -
                    // read as a startup timeout. A periodic IDR lets segments cut.
                    "-g".into(),
                    GOP_FRAMES.to_string(),
                ]);
            }
            encode.extend_from_slice(&[
                "-rc".into(),
                "vbr".into(),
                "-cq".into(),
                config.ffmpeg_video_crf.to_string(),
                "-b:v".into(),
                "0".into(),
            ]);
            VideoPipeline {
                // Decode on the GPU and keep frames in VRAM.
                pre_input: vec![
                    "-hwaccel".into(),
                    "cuda".into(),
                    "-hwaccel_output_format".into(),
                    "cuda".into(),
                ],
                // Convert 10-bit (p010) → 8-bit (nv12) on the GPU; h264_nvenc is
                // 8-bit only. Doing it here avoids the CPU `-pix_fmt` round-trip.
                filter: vec!["-vf".into(), "scale_cuda=format=nv12".into()],
                encode,
            }
        } else {
            let mut encode: Vec<String> = vec![
                "-c:v".into(),
                "libx264".into(),
                "-preset".into(),
                config.ffmpeg_video_preset.clone(),
            ];
            if low_latency {
                encode.extend_from_slice(&["-tune".into(), "zerolatency".into()]);
            }
            encode.extend_from_slice(&[
                "-crf".into(),
                config.ffmpeg_video_crf.to_string(),
                // Match the segment cadence so the first segment closes quickly
                // rather than at libx264's default ~250-frame keyint.
                "-g".into(),
                GOP_FRAMES.to_string(),
                "-pix_fmt".into(),
                "yuv420p".into(),
                "-bf".into(),
                "0".into(),
                "-b_strategy".into(),
                "0".into(),
            ]);
            VideoPipeline {
                pre_input: if config.ffmpeg_hwaccel != "none" {
                    vec!["-hwaccel".into(), config.ffmpeg_hwaccel.clone()]
                } else {
                    Vec::new()
                },
                filter: Vec::new(),
                encode,
            }
        }
    }
}

/// Whether the `h264_nvenc` hardware encoder is usable (compiled in AND backed
/// by a working GPU). Probed once via a trivial test encode and cached, since a
/// machine whose ffmpeg lists nvenc may still lack an NVIDIA device.
static NVENC_AVAILABLE: tokio::sync::OnceCell<bool> = tokio::sync::OnceCell::const_new();

async fn nvenc_available() -> bool {
    *NVENC_AVAILABLE
        .get_or_init(|| async {
            let status = tokio::process::Command::new("ffmpeg")
                .args([
                    "-hide_banner",
                    "-loglevel",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    "nullsrc=s=256x256:d=0.1",
                    "-c:v",
                    "h264_nvenc",
                    "-f",
                    "null",
                    "-",
                ])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .await;
            let ok = matches!(status, Ok(s) if s.success());
            if ok {
                tracing::info!("h264_nvenc hardware encoder available");
            } else {
                tracing::info!("h264_nvenc unavailable; falling back to software libx264");
            }
            ok
        })
        .await
}
