//! Smoke test for the non-HLS entry points:
//! `cargo run -p media --example tools_check -- <video> <subtitle-file> <trailer>`.

use std::sync::Arc;
use std::time::{Duration, Instant};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();

    let t = Instant::now();
    let input = media::Input::File(args[2].clone().into());
    let cues = media::extract_subtitles(&input, 1, Duration::from_secs(30)).await?;
    println!(
        "subtitles: {} cues in {:.2}s, first: {:?}",
        cues.len(),
        t.elapsed().as_secs_f64(),
        cues.iter().take(2).collect::<Vec<_>>()
    );

    let t = Instant::now();
    let aspect = media::content_aspect(std::path::Path::new(&args[3])).await;
    println!("aspect: {aspect:?} in {:.2}s", t.elapsed().as_secs_f64());

    let input = media::Input::File(args[1].clone().into());
    let info = Arc::new(media::probe(&input).await?);
    let output = std::env::temp_dir().join("tools_check_out.mp4");
    let t = Instant::now();
    let job = media::file::FileTranscode::start(media::file::TranscodeOptions {
        input,
        info,
        video: media::VideoAction::Transcode,
        audio: media::AudioAction::Transcode,
        audio_index: 0,
        encoder: media::EncoderSettings::default(),
        output: output.clone(),
    })?;
    tokio::time::sleep(Duration::from_millis(500)).await;
    job.pause();
    let paused_at = job.position();
    tokio::time::sleep(Duration::from_millis(500)).await;
    println!("paused at {paused_at:?}, still {:?}", job.position());
    job.resume();
    job.wait().await?;
    println!("transcoded in {:.2}s", t.elapsed().as_secs_f64());
    let out = media::probe(&media::Input::File(output.clone())).await?;
    println!(
        "output: {:?} {:?} {:?} {:?}",
        out.container,
        out.duration,
        out.video.first().map(|v| (&v.codec, v.width, v.height)),
        out.audio.first().map(|a| (&a.codec, a.channels))
    );
    println!(
        "output keyframes: {:?}",
        media::keyframes(&media::Input::File(output))
            .await?
            .map(|k| k.len())
    );
    Ok(())
}
