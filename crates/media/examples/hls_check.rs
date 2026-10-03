//! Exercises the HLS packager against a real file the way a player would:
//! `cargo run -p media --example hls_check -- <file> [copy|transcode] [stream]`.
//!
//! Prints the probe, then fetches segments in playback order, across a far
//! seek and back, checking each one starts where the playlist says it does.

use std::sync::Arc;
use std::time::Instant;

use media::hls::{Session, SessionOptions, Track};

struct FileOpener(std::path::PathBuf, u64);

impl media::Opener for FileOpener {
    fn size(&self) -> u64 {
        self.1
    }
    fn open(&self) -> futures::future::BoxFuture<'static, std::io::Result<media::BoxReader>> {
        let path = self.0.clone();
        Box::pin(async move {
            let f = tokio::fs::File::open(path).await?;
            Ok(Box::pin(f) as media::BoxReader)
        })
    }
}

fn tfdt_seconds(init: &[u8], frag: &[u8]) -> Option<f64> {
    fn find<'a>(data: &'a [u8], name: &[u8; 4]) -> Option<&'a [u8]> {
        let mut i = 0;
        while i + 8 <= data.len() {
            let size = u32::from_be_bytes(data[i..i + 4].try_into().ok()?) as usize;
            if size < 8 || i + size > data.len() {
                return None;
            }
            let body = &data[i + 8..i + size];
            if &data[i + 4..i + 8] == name {
                return Some(body);
            }
            if matches!(
                &data[i + 4..i + 8],
                b"moof" | b"traf" | b"moov" | b"trak" | b"mdia"
            ) && let Some(b) = find(body, name)
            {
                return Some(b);
            }
            i += size;
        }
        None
    }
    let mdhd = find(init, b"mdhd")?;
    let ts = if mdhd[0] == 1 {
        u32::from_be_bytes(mdhd[20..24].try_into().ok()?)
    } else {
        u32::from_be_bytes(mdhd[12..16].try_into().ok()?)
    };
    let t = find(frag, b"tfdt")?;
    let d = if t[0] == 1 {
        u64::from_be_bytes(t[4..12].try_into().ok()?)
    } else {
        u32::from_be_bytes(t[4..8].try_into().ok()?) as u64
    };
    Some(d as f64 / ts as f64)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let path = std::path::PathBuf::from(&args[1]);
    let mode = args.get(2).map(String::as_str).unwrap_or("copy");
    let input = if args.get(3).map(String::as_str) == Some("stream") {
        let len = std::fs::metadata(&path)?.len();
        media::Input::stream(Arc::new(FileOpener(path.clone(), len)), "test")
    } else {
        media::Input::File(path.clone())
    };

    let t = Instant::now();
    let info = media::probe(&input).await?;
    println!(
        "probe {:.2}s: {:?} {:?}",
        t.elapsed().as_secs_f64(),
        info.container,
        info.duration
    );
    for v in &info.video {
        println!(
            "  video {} {}x{} {}bit {:?}",
            v.codec, v.width, v.height, v.bit_depth, v.mime_codec
        );
    }
    for a in &info.audio {
        println!(
            "  audio#{} {} {}ch {:?} {:?} {:?}",
            a.stream_index, a.codec, a.channels, a.language, a.title, a.mime_codec
        );
    }
    for s in &info.subtitles {
        println!(
            "  sub#{} {} text={} {:?} {:?}",
            s.stream_index, s.codec, s.text, s.language, s.title
        );
    }
    println!("  {} chapters", info.chapters.len());

    let t = Instant::now();
    let kf = media::keyframes(&input).await?;
    println!(
        "keyframes {:.2}s: {:?}",
        t.elapsed().as_secs_f64(),
        kf.as_ref().map(|k| (
            k.len(),
            k.iter()
                .take(4)
                .map(|t| *t as f64 / 1e9)
                .collect::<Vec<_>>()
        ))
    );

    let (video, audio) = match mode {
        "copy" => (media::VideoAction::Copy, media::AudioAction::Transcode),
        _ => (media::VideoAction::Transcode, media::AudioAction::Transcode),
    };
    let dir = std::env::temp_dir().join(format!("hls_check_{}", std::process::id()));
    let session = Session::new(SessionOptions {
        input,
        info: Arc::new(info),
        video,
        audio,
        audio_index: 0,
        encoder: media::EncoderSettings {
            // `x264` as the fourth argument forces software encoding.
            hardware: if args.get(4).map(String::as_str) == Some("x264") {
                media::Hardware::None
            } else {
                media::Hardware::Auto
            },
            ..Default::default()
        },
        dir,
        on_seek: None,
    })
    .await?;
    let n = session.segment_count();
    println!("{n} segments over {:?}", session.duration());
    print!("{}", session.master_playlist());
    let playlist = session.media_playlist(Track::Video);
    let starts: Vec<f64> = {
        let mut t = 0.0;
        let mut v = Vec::new();
        for line in playlist.lines() {
            if let Some(d) = line.strip_prefix("#EXTINF:") {
                v.push(t);
                t += d.trim_end_matches(',').parse::<f64>().unwrap();
            }
        }
        v
    };

    let init_v = session.init(Track::Video).await?;
    let init_a = session.init(Track::Audio).await?;
    let far = n / 5;
    let order: Vec<usize> = [0, 1, 2, 3, far, far + 1, far + 2, 1, 4]
        .into_iter()
        .filter(|&i| i < n)
        .collect();
    let mut worst: f64 = 0.0;
    for i in order {
        for (track, init) in [(Track::Video, &init_v), (Track::Audio, &init_a)] {
            let t = Instant::now();
            let seg = session.segment(track, i).await?;
            let start = tfdt_seconds(init, &seg).unwrap_or(f64::NAN);
            let off = start - starts[i];
            worst = worst.max(off.abs());
            let dur = starts.get(i + 1).copied().unwrap_or(start + 4.0) - starts[i];
            println!(
                "{track:?} {i:>4}: {:>8} bytes ({:>5.1} Mbit/s) in {:>6.3}s, starts {start:>9.3} (playlist {:>9.3}, {off:+.3})",
                seg.len(),
                seg.len() as f64 * 8.0 / dur / 1e6,
                t.elapsed().as_secs_f64(),
                starts[i],
            );
        }
    }
    println!("worst start offset {worst:.3}s");
    Ok(())
}
