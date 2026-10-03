//! `cargo run -p media --example transcode_file -- <input> <output.mp4>`:
//! a full re-encode through the background-transcode path (tone mapping HDR).
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let a: Vec<String> = std::env::args().collect();
    let input = media::Input::File(a[1].clone().into());
    let info = std::sync::Arc::new(media::probe(&input).await?);
    println!(
        "source: {:?}",
        info.video
            .first()
            .map(|v| (&v.codec, v.width, v.height, v.bit_depth, v.hdr))
    );
    let t = std::time::Instant::now();
    let job = media::file::FileTranscode::start(media::file::TranscodeOptions {
        input,
        info: info.clone(),
        video: media::VideoAction::Transcode,
        audio: media::AudioAction::Transcode,
        audio_index: 0,
        encoder: media::EncoderSettings::default(),
        output: a[2].clone().into(),
    })?;
    job.wait().await?;
    let secs = t.elapsed().as_secs_f64();
    let dur = info.duration.map(|d| d.as_secs_f64()).unwrap_or(0.0);
    println!(
        "transcoded {dur:.1}s of video in {secs:.2}s ({:.1}x real time)",
        dur / secs
    );
    Ok(())
}
