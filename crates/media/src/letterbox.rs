//! The aspect ratio of a video's picture once black bars are cropped away.

use std::path::Path;

use gst::prelude::*;

use crate::pipeline::{capsfilter, make};

/// Rows or columns averaging below this luma count as bar.
const BLACK: u32 = 24;
/// Frames sampled, like `cropdetect` over ~12s.
const FRAMES: usize = 300;

/// Samples frames from 3s in and returns the widest content box's width /
/// height. `None` if the file can't be decoded.
pub async fn content_aspect(path: &Path) -> Option<f64> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || detect(&path))
        .await
        .ok()
        .flatten()
}

fn detect(path: &Path) -> Option<f64> {
    crate::init().ok()?;
    let pipeline = gst::Pipeline::new();
    let result = (|| {
        let src = gst::ElementFactory::make("filesrc")
            .property("location", path.to_string_lossy().as_ref())
            .build()
            .ok()?;
        let decode = make("decodebin").ok()?;
        // A few hundred small frames; hardware decoders would only drag in GL.
        decode.set_property("force-sw-decoders", true);
        let convert = make("videoconvertscale")
            .or_else(|_| make("videoconvert"))
            .ok()?;
        // Square pixels at a fixed width, so the box ratio is the display ratio.
        let caps = capsfilter("video/x-raw,format=GRAY8,width=480,pixel-aspect-ratio=1/1").ok()?;
        let sink = gst_app::AppSink::builder()
            .sync(false)
            .max_buffers(4)
            .build();
        pipeline
            .add_many([&src, &decode, &convert, &caps, sink.upcast_ref()])
            .ok()?;
        src.link(&decode).ok()?;
        gst::Element::link_many([&convert, &caps, sink.upcast_ref()]).ok()?;
        let convert_weak = convert.downgrade();
        decode.connect_pad_added(move |_, pad| {
            if let Some(convert) = convert_weak.upgrade()
                && pad.current_caps().is_some_and(|c| {
                    c.structure(0)
                        .is_some_and(|s| s.name().starts_with("video/"))
                })
            {
                let _ = pad.link(&convert.static_pad("sink").unwrap());
            }
        });

        pipeline.set_state(gst::State::Paused).ok()?;
        pipeline.state(gst::ClockTime::from_seconds(10)).0.ok()?;
        let _ = pipeline.seek_simple(
            gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
            gst::ClockTime::from_seconds(3),
        );
        pipeline.set_state(gst::State::Playing).ok()?;

        let mut best: Option<(usize, usize)> = None;
        for _ in 0..FRAMES {
            let Some(sample) = sink.try_pull_sample(gst::ClockTime::from_seconds(5)) else {
                break;
            };
            let info = sample
                .caps()
                .and_then(|c| gst_video::VideoInfo::from_caps(c).ok())?;
            let buffer = sample.buffer()?;
            let map = buffer.map_readable().ok()?;
            if let Some((w, h)) = content_box(
                &map,
                info.width() as usize,
                info.height() as usize,
                info.stride()[0] as usize,
            ) && best.is_none_or(|(bw, bh)| (h, w) > (bh, bw))
            {
                best = Some((w, h));
            }
        }
        best.filter(|&(w, h)| w > 0 && h > 0)
            .map(|(w, h)| w as f64 / h as f64)
    })();
    let _ = pipeline.set_state(gst::State::Null);
    result
}

/// Width and height of the non-black region of a GRAY8 frame.
fn content_box(data: &[u8], width: usize, height: usize, stride: usize) -> Option<(usize, usize)> {
    if data.len() < stride * height || width == 0 {
        return None;
    }
    let row_lit = |y: usize| {
        let row = &data[y * stride..y * stride + width];
        row.iter().map(|&p| p as u32).sum::<u32>() / width as u32 > BLACK
    };
    let col_lit = |x: usize, top: usize, bottom: usize| {
        let sum: u32 = (top..bottom).map(|y| data[y * stride + x] as u32).sum();
        sum / (bottom - top).max(1) as u32 > BLACK
    };
    let top = (0..height).find(|&y| row_lit(y))?;
    let bottom = (0..height).rev().find(|&y| row_lit(y))? + 1;
    let left = (0..width).find(|&x| col_lit(x, top, bottom))?;
    let right = (0..width).rev().find(|&x| col_lit(x, top, bottom))? + 1;
    Some((right - left, bottom - top))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_letterboxed_picture() {
        let (w, h) = (100, 60);
        let mut frame = vec![16u8; w * h];
        for y in 10..50 {
            for x in 0..w {
                frame[y * w + x] = 120;
            }
        }
        assert_eq!(content_box(&frame, w, h, w), Some((100, 40)));
    }
}
