pub(crate) async fn transcode(
    config: &crate::Config,
    source: &crate::downloads::MediaSource,
    copy_video: bool,
    start_time: f64,
    audio_index: usize,
    playlist_path: &std::path::Path,
    segment_pattern: &std::path::Path,
) -> tokio::process::Command {
    let video = super::pipeline::VideoPipeline::new(config, copy_video, true).await;

    // Decode-side args precede `-i`: the video hwaccel, then input seeking.
    let mut pre_args = video.pre_input.clone();
    if start_time > 0.0 {
        pre_args.extend_from_slice(&["-ss".into(), format!("{:.3}", start_time), "-copyts".into()]);
    }

    let mut command = tokio::process::Command::new("ffmpeg");
    command.args(&pre_args);
    // Disk sources read the file directly (cheap, no pump). Engine sources
    // pipe through stdin so ffmpeg blocks on missing pieces rather than hitting
    // premature EOF on a partial file.
    match source.ffmpeg_input_spec() {
        crate::downloads::FfmpegInputSpec::Path(p) => {
            command.arg("-i").arg(p);
            command.stdin(std::process::Stdio::null());
        }
        crate::downloads::FfmpegInputSpec::Pipe => {
            command.args(["-i", "pipe:0"]);
            command.stdin(std::process::Stdio::piped());
        }
    }
    command
        .args(["-map", "0:v:0", "-map", &format!("0:a:{}", audio_index)])
        .args(&video.filter)
        .args(&video.encode)
        .args([
            "-c:a",
            "aac",
            "-b:a",
            "192k",
            "-ac",
            "2",
            "-af",
            "aresample=async=1:first_pts=0",
            "-f",
            "hls",
            // Short segments so the first one needs far less input data — critical
            // for a cold torrent (have 0) where ffmpeg blocks on pipe:0 waiting for
            // the head of the file to download before it can emit segment 0.
            "-hls_time",
            "2",
            "-hls_list_size",
            "0",
            "-hls_flags",
            "append_list",
            "-hls_segment_filename",
            segment_pattern.to_str().unwrap_or(""),
            "-hls_playlist_type",
            "event",
        ])
        .arg(playlist_path.to_str().unwrap_or(""))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);

    command
}

pub(crate) fn local_transcode(
    start_time: f64,
    path: &std::path::Path,
    playlist_path: &std::path::Path,
    segment_pattern: &std::path::Path,
) -> tokio::process::Command {
    // `-ss` before `-i` gives fast keyframe seek because the moov atom is at
    // the head of the file (pretranscodings write with `-movflags +faststart`).
    let mut pre_args: Vec<String> = Vec::new();
    let mut post_args: Vec<String> = Vec::new();
    if start_time > 0.0 {
        pre_args.extend_from_slice(&["-ss".into(), format!("{start_time:.3}")]);
        post_args.extend_from_slice(&[
            "-copyts".into(),
            "-output_ts_offset".into(),
            format!("-{start_time:.3}"),
        ]);
    }

    let mut cmd = tokio::process::Command::new("ffmpeg");
    cmd.args(&pre_args)
        .args(["-i".to_string()])
        .arg(path)
        .args(&post_args)
        .args(["-map", "0:v:0", "-map", "0:a:0"])
        .args([
            "-c:v",
            "copy",
            "-c:a",
            "copy",
            "-f",
            "hls",
            "-hls_time",
            "2",
            "-hls_list_size",
            "0",
            "-hls_flags",
            "append_list",
            "-hls_segment_filename",
            segment_pattern.to_str().unwrap_or(""),
            "-hls_playlist_type",
            "event",
        ])
        .arg(playlist_path.to_str().unwrap_or(""))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);

    cmd
}

pub(crate) async fn pretranscode(
    config: &crate::Config,
    source: &crate::downloads::MediaSource,
    copy_video: bool,
    audio_index: i32,
    path: &std::path::Path,
) -> tokio::process::Command {
    let video = super::pipeline::VideoPipeline::new(config, copy_video, false).await;

    // Pretranscodes run at higher-than-live quality settings by default.
    let mut cmd = tokio::process::Command::new("ffmpeg");
    cmd.args(&video.pre_input).args([
        "-hide_banner",
        "-loglevel",
        "warning",
        "-nostats",
        "-progress",
        "pipe:1",
        "-y",
    ]);
    match source.ffmpeg_input_spec() {
        crate::downloads::FfmpegInputSpec::Path(p) => {
            cmd.arg("-i").arg(p);
            cmd.stdin(std::process::Stdio::null());
        }
        crate::downloads::FfmpegInputSpec::Pipe => {
            cmd.args(["-i", "pipe:0"]);
            cmd.stdin(std::process::Stdio::piped());
        }
    }
    cmd.args(["-map", "0:v:0", "-map", &format!("0:a:{audio_index}")])
        .args(&video.filter)
        .args(&video.encode)
        .args([
            "-c:a",
            "aac",
            "-b:a",
            "192k",
            "-ac",
            "2",
            "-af",
            "aresample=async=1:first_pts=0",
            "-movflags",
            "+faststart",
            "-f",
            "mp4",
        ])
        .arg(path)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);

    cmd
}
