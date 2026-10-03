//! `cargo run -p media --example mux_check -- <video> <audio> <out.mp4>`
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let a: Vec<String> = std::env::args().collect();
    let t = std::time::Instant::now();
    media::file::mux_mp4(a[1].as_ref(), Some(a[2].as_ref()), a[3].as_ref()).await?;
    println!("muxed in {:.2}s", t.elapsed().as_secs_f64());
    let info = media::probe(&media::Input::File(a[3].clone().into())).await?;
    println!(
        "{:?} {:?} video={:?} audio={:?}",
        info.container,
        info.duration,
        info.video.first().map(|v| (&v.codec, v.height)),
        info.audio.first().map(|a| (&a.codec, a.channels))
    );
    let head = std::fs::read(&a[3])?;
    let boxes: Vec<String> = (0..4)
        .scan(0usize, |at, _| {
            let size = u32::from_be_bytes(head.get(*at..*at + 4)?.try_into().ok()?) as usize;
            let name = String::from_utf8_lossy(head.get(*at + 4..*at + 8)?).to_string();
            *at += size.max(8);
            Some(name)
        })
        .collect();
    println!("top-level boxes: {boxes:?}");
    Ok(())
}
