//! Text cues from a subtitle stream embedded in a file.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use gst::prelude::*;

use crate::pipeline::{self, make};
use crate::{Error, Input, Result};

#[derive(Clone, Debug, PartialEq)]
pub struct Cue {
    /// Seconds.
    pub start: f64,
    pub end: f64,
    pub text: String,
}

/// Cues of the `stream_index`-th subtitle stream. Subtitles are interleaved
/// through the whole file, so this demuxes all of it; for a torrent still
/// downloading, whatever arrives before `timeout` is returned.
pub async fn extract_subtitles(
    input: &Input,
    stream_index: usize,
    timeout: Duration,
) -> Result<Vec<Cue>> {
    crate::init()?;
    let pipeline = gst::Pipeline::new();
    let cues: Arc<Mutex<Vec<Cue>>> = Default::default();
    let result = run(&pipeline, input, stream_index, timeout, cues.clone()).await;
    pipeline::shutdown(pipeline);
    let cues = std::mem::take(&mut *cues.lock().unwrap());
    match result {
        Ok(()) | Err(Error::Timeout(_)) => Ok(cues),
        Err(err) if !cues.is_empty() => {
            tracing::debug!(%err, "Subtitle extraction stopped early");
            Ok(cues)
        }
        Err(err) => Err(err),
    }
}

async fn run(
    pipeline: &gst::Pipeline,
    input: &Input,
    stream_index: usize,
    timeout: Duration,
    cues: Arc<Mutex<Vec<Cue>>>,
) -> Result<()> {
    let src = input.source_element()?;
    let parsebin = make("parsebin")?;
    pipeline.add_many([&src, &parsebin])?;
    src.link(&parsebin)?;

    // The stream to extract, by id: the `stream_index`-th subtitle stream in
    // the demuxer's order, the order the probe numbers tracks in. parsebin
    // announces the collection before it exposes any pad.
    let wanted: Arc<Mutex<Option<String>>> = Default::default();
    let wanted_bus = wanted.clone();
    let bus = pipeline.bus().expect("pipeline bus");
    bus.set_sync_handler(move |_, msg| {
        if let gst::MessageView::StreamCollection(sc) = msg.view() {
            *wanted_bus.lock().unwrap() = sc
                .stream_collection()
                .iter()
                .filter(|s| s.stream_type().contains(gst::StreamType::TEXT))
                .nth(stream_index)
                .and_then(|s| s.stream_id())
                .map(|id| id.to_string());
        }
        gst::BusSyncReply::Pass
    });

    let pipeline_weak = pipeline.downgrade();
    parsebin.connect_pad_added(move |_, pad| {
        let Some(pipeline) = pipeline_weak.upgrade() else {
            return;
        };
        let caps = pad.current_caps().unwrap_or_else(|| pad.query_caps(None));
        let name = caps
            .structure(0)
            .map(|s| s.name().to_string())
            .unwrap_or_default();
        let id = pad
            .stream()
            .and_then(|s| s.stream_id())
            .map(|id| id.to_string());
        let mine = id.is_some() && id == *wanted.lock().unwrap();

        let sink: gst::Element = if mine && pipeline::is_text_subtitle(&caps) {
            let format = caps
                .structure(0)
                .and_then(|s| s.get::<String>("format").ok());
            let kind = TextKind::from_caps(&name, format.as_deref());
            let cues = cues.clone();
            // No sink prerolls: they share the demuxer's thread, and one waiting
            // for its first buffer would stall all the others.
            let sink = gst_app::AppSink::builder()
                .sync(false)
                .async_(false)
                .callbacks(
                    gst_app::AppSinkCallbacks::builder()
                        .new_sample(move |sink| {
                            let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                            if let (Some(buffer), Some(segment)) =
                                (sample.buffer(), sample.segment())
                                && let Some(cue) = cue_from(buffer, segment, kind)
                            {
                                cues.lock().unwrap().push(cue);
                            }
                            Ok(gst::FlowSuccess::Ok)
                        })
                        .build(),
                )
                .build();
            sink.upcast()
        } else {
            match gst::ElementFactory::make("fakesink")
                .property("sync", false)
                .property("async", false)
                .build()
            {
                Ok(s) => s,
                Err(_) => return,
            }
        };
        if pipeline.add(&sink).is_ok() {
            let _ = sink.sync_state_with_parent();
            let _ = pad.link(&sink.static_pad("sink").unwrap());
        }
    });

    pipeline.set_state(gst::State::Playing)?;
    pipeline::wait_bus(&bus, timeout, "extracting subtitles", |msg| {
        matches!(msg.view(), gst::MessageView::Eos(_)).then_some(())
    })
    .await
}

#[derive(Clone, Copy)]
enum TextKind {
    Plain,
    Pango,
    Ass,
}

impl TextKind {
    fn from_caps(name: &str, format: Option<&str>) -> Self {
        match (name, format) {
            ("application/x-ssa" | "application/x-ass", _) => TextKind::Ass,
            ("text/x-raw", Some("pango-markup")) => TextKind::Pango,
            _ => TextKind::Plain,
        }
    }
}

fn cue_from(buffer: &gst::BufferRef, segment: &gst::Segment, kind: TextKind) -> Option<Cue> {
    let segment = segment.downcast_ref::<gst::ClockTime>()?;
    let pts = buffer.pts()?;
    let start = segment.to_stream_time(pts)?;
    let end = start + buffer.duration().unwrap_or(gst::ClockTime::from_seconds(3));
    let map = buffer.map_readable().ok()?;
    let raw = String::from_utf8_lossy(&map);
    let text = match kind {
        TextKind::Plain => strip_tags(&raw),
        TextKind::Pango => unescape(&strip_tags(&raw)),
        TextKind::Ass => ass_text(&raw),
    };
    let text = text.replace("\r\n", "\n").trim().to_string();
    (!text.is_empty()).then(|| Cue {
        start: start.nseconds() as f64 / 1e9,
        end: end.nseconds() as f64 / 1e9,
        text,
    })
}

fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// The text of a Matroska ASS/SSA event (`ReadOrder,Layer,Style,Name,
/// MarginL,MarginR,MarginV,Effect,Text`), without override blocks.
fn ass_text(event: &str) -> String {
    let text = event.splitn(9, ',').nth(8).unwrap_or(event);
    let mut out = String::with_capacity(text.len());
    let mut depth = 0;
    for c in text.chars() {
        match c {
            '{' => depth += 1,
            '}' if depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out.replace("\\N", "\n")
        .replace("\\n", "\n")
        .replace("\\h", " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ass_events_lose_fields_and_overrides() {
        assert_eq!(
            ass_text("12,0,Default,,0,0,0,,{\\i1}Hello,{\\i0} there\\Nfriend"),
            "Hello, there\nfriend"
        );
    }

    #[test]
    fn pango_markup_is_plain_text() {
        assert_eq!(
            unescape(&strip_tags("<i>Tom &amp; Jerry</i>")),
            "Tom & Jerry"
        );
    }
}
