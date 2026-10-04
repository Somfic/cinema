//! What's in a file: container, streams with their codecs and tags, duration
//! and chapters.
//!
//! Demuxes just far enough to see every stream: `parsebin` announces how many
//! streams there are, exposes a pad for each, and once every pad carries
//! fixed caps the streams' tags have arrived with them. The sinks don't preroll:
//! all pads share the demuxer's thread, and a prerolled sink would block it
//! before a sparse subtitle stream ever got its first buffer.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use gst::prelude::*;

use crate::pipeline::{codec_name, is_text_subtitle, make, wait_bus};
use crate::{Input, Result};

/// How long a probe may take. A file on disk answers in milliseconds; a
/// torrent still downloading waits for the swarm to deliver the header.
const PROBE_TIMEOUT_FILE: Duration = Duration::from_secs(30);
const PROBE_TIMEOUT_STREAM: Duration = Duration::from_secs(600);

#[derive(Clone, Debug, Default)]
pub struct MediaInfo {
    pub duration: Option<Duration>,
    /// `matroska`, `mp4`, `webm`, `mpegts`, ...
    pub container: Option<String>,
    pub video: Vec<VideoTrack>,
    pub audio: Vec<AudioTrack>,
    pub subtitles: Vec<SubtitleTrack>,
    pub chapters: Vec<Chapter>,
}

#[derive(Clone, Debug)]
pub struct VideoTrack {
    pub codec: String,
    pub width: u32,
    pub height: u32,
    pub bit_depth: u32,
    /// `4:2:0`, `4:2:2`, `4:4:4`, when the parser reports it.
    pub chroma_format: Option<String>,
    /// Set for HDR video, by transfer function.
    pub hdr: Option<crate::Hdr>,
    /// RFC 6381 codec string (`avc1.640028`), for HLS `CODECS`.
    pub mime_codec: Option<String>,
}

#[derive(Clone, Debug)]
pub struct AudioTrack {
    /// Position among all streams of the file.
    pub index: usize,
    /// Position among the audio streams; what callers select with.
    pub stream_index: usize,
    pub codec: String,
    pub channels: u32,
    pub language: Option<String>,
    pub title: Option<String>,
    pub mime_codec: Option<String>,
}

#[derive(Clone, Debug)]
pub struct SubtitleTrack {
    pub index: usize,
    /// Position among the subtitle streams; what callers select with.
    pub stream_index: usize,
    pub codec: String,
    pub language: Option<String>,
    pub title: Option<String>,
    /// Text cues (as opposed to bitmaps like PGS).
    pub text: bool,
}

#[derive(Clone, Debug)]
pub struct Chapter {
    pub start: Duration,
    pub end: Duration,
    pub title: Option<String>,
}

pub async fn probe(input: &Input) -> Result<MediaInfo> {
    crate::init()?;
    let pipeline = gst::Pipeline::new();
    let result = run(&pipeline, input).await;
    crate::pipeline::shutdown(pipeline);
    result
}

async fn run(pipeline: &gst::Pipeline, input: &Input) -> Result<MediaInfo> {
    let src = input.source_element()?;
    let typefind = make("typefind")?;
    let parsebin = make("parsebin")?;
    pipeline.add_many([&src, &typefind, &parsebin])?;
    gst::Element::link_many([&src, &typefind, &parsebin])?;

    let pipeline_weak = pipeline.downgrade();
    parsebin.connect_pad_added(move |_, pad| {
        let Some(pipeline) = pipeline_weak.upgrade() else {
            return;
        };
        let Ok(sink) = gst::ElementFactory::make("fakesink")
            .property("sync", false)
            .property("async", false)
            .build()
        else {
            return;
        };
        if pipeline.add(&sink).is_ok() {
            let _ = sink.sync_state_with_parent();
            let _ = pad.link(&sink.static_pad("sink").unwrap());
        }
    });

    let bus = pipeline.bus().expect("pipeline bus");
    pipeline.set_state(gst::State::Playing)?;

    let toc: Arc<Mutex<Option<gst::Toc>>> = Default::default();
    let toc_slot = toc.clone();
    let collection: Arc<Mutex<Option<gst::StreamCollection>>> = Default::default();
    let streams = collection.clone();
    let streams_slot = collection.clone();
    let parsebin_ready = parsebin.clone();
    let ready = async move {
        // Caps settle once each parser has seen its stream's first frames.
        loop {
            let expected = streams.lock().unwrap().as_ref().map(|c| c.len());
            let pads = parsebin_ready.src_pads();
            if let Some(expected) = expected
                && pads.len() >= expected
                && pads
                    .iter()
                    .all(|p| p.current_caps().is_some_and(|c| c.is_fixed()))
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    tokio::pin!(ready);
    let timeout = match input {
        Input::File(_) => PROBE_TIMEOUT_FILE,
        Input::Stream { .. } => PROBE_TIMEOUT_STREAM,
    };
    let watch = wait_bus(&bus, timeout, "probing media", |msg| {
        match msg.view() {
            gst::MessageView::Toc(t) => *toc_slot.lock().unwrap() = Some(t.toc().0),
            gst::MessageView::StreamCollection(c) => {
                *streams_slot.lock().unwrap() = Some(c.stream_collection());
            }
            _ => {}
        }
        // Reaching the end first is fine: everything has been seen.
        matches!(msg.view(), gst::MessageView::Eos(_)).then_some(())
    });
    tokio::select! {
        _ = &mut ready => {}
        r = watch => r?,
    }
    // The TOC message trails the header parse by a moment.
    if toc.lock().unwrap().is_none() {
        tokio::time::sleep(Duration::from_millis(50)).await;
        while let Some(msg) = bus.pop_filtered(&[gst::MessageType::Toc]) {
            if let gst::MessageView::Toc(t) = msg.view() {
                *toc.lock().unwrap() = Some(t.toc().0);
            }
        }
    }
    let toc = toc.lock().unwrap().take();

    let mut info = MediaInfo {
        duration: pipeline
            .query_duration::<gst::ClockTime>()
            .map(|d| Duration::from_nanos(d.nseconds())),
        container: detected_container(pipeline),
        ..Default::default()
    };

    // Index streams in the demuxer's own order (its stream collection), the
    // order playback selects tracks by. parsebin numbers its pads by when
    // each parser finished, which differs from run to run.
    let pads = parsebin.src_pads();
    let ordered: Vec<gst::Pad> = match collection.lock().unwrap().as_ref() {
        Some(collection) => collection
            .iter()
            .filter_map(|stream| {
                let id = stream.stream_id()?;
                pads.iter()
                    .find(|p| p.stream().and_then(|s| s.stream_id()).as_ref() == Some(&id))
                    .cloned()
            })
            .collect(),
        None => pads,
    };

    for (index, pad) in ordered.iter().enumerate() {
        let Some(caps) = pad.current_caps() else {
            continue;
        };
        let Some(s) = caps.structure(0) else { continue };
        let tags = stream_tags(pad);
        let language = tags
            .as_ref()
            .and_then(|t| {
                t.get::<gst::tags::LanguageCode>()
                    .map(|v| v.get().to_string())
            })
            .map(|l| iso639_2(&l));
        let title = tags
            .as_ref()
            .and_then(|t| t.get::<gst::tags::Title>().map(|v| v.get().to_string()))
            .filter(|t| !t.trim().is_empty());
        let mime_codec = gst_pbutils::codec_utils_caps_get_mime_codec(&caps)
            .ok()
            .map(|s| s.to_string());

        let kind = s.name().as_str();
        if kind.starts_with("video/") || kind.starts_with("image/") {
            info.video.push(VideoTrack {
                codec: codec_name(&caps),
                width: s.get::<i32>("width").unwrap_or(0) as u32,
                height: s.get::<i32>("height").unwrap_or(0) as u32,
                bit_depth: s
                    .get::<u32>("bit-depth-luma")
                    .ok()
                    .or_else(|| profile_bit_depth(s.get::<&str>("profile").ok()))
                    .unwrap_or(8),
                chroma_format: s.get::<String>("chroma-format").ok(),
                hdr: crate::Hdr::from_caps(&caps),
                mime_codec,
            });
        } else if kind.starts_with("audio/") {
            info.audio.push(AudioTrack {
                index,
                stream_index: info.audio.len(),
                codec: codec_name(&caps),
                channels: s.get::<i32>("channels").unwrap_or(2) as u32,
                language,
                title,
                mime_codec,
            });
        } else if kind.starts_with("text/")
            || kind.starts_with("subpicture/")
            || kind.starts_with("application/x-s")
            || kind.starts_with("application/x-subtitle")
        {
            info.subtitles.push(SubtitleTrack {
                index,
                stream_index: info.subtitles.len(),
                codec: codec_name(&caps),
                language,
                title,
                text: is_text_subtitle(&caps),
            });
        }
    }

    if let Some(toc) = toc {
        info.chapters = chapters(&toc, info.duration);
    }

    Ok(info)
}

/// Per-stream tags: the stream object's own, else the pad's sticky tag event.
fn stream_tags(pad: &gst::Pad) -> Option<gst::TagList> {
    if let Some(tags) = pad.stream().and_then(|s| s.tags())
        && tags.n_tags() > 0
    {
        return Some(tags);
    }
    (0..)
        .map_while(|i| pad.sticky_event::<gst::event::Tag>(i))
        .map(|ev| ev.tag_owned())
        .find(|t| t.scope() == gst::TagScope::Stream)
}

fn profile_bit_depth(profile: Option<&str>) -> Option<u32> {
    let p = profile?;
    if p.contains("10") {
        Some(10)
    } else if p.contains("12") {
        Some(12)
    } else {
        None
    }
}

/// The container type found by typefinding: ours, or the one parsebin runs.
fn detected_container(pipeline: &gst::Pipeline) -> Option<String> {
    pipeline
        .iterate_recurse()
        .into_iter()
        .flatten()
        .filter(|e| e.factory().is_some_and(|f| f.name() == "typefind"))
        .find_map(|e| e.property::<Option<gst::Caps>>("caps"))
        .and_then(|c| c.structure(0).map(|s| container_name(s.name())))
}

/// Short container names by their typefind caps.
fn container_name(caps_name: &str) -> String {
    match caps_name {
        "video/x-matroska" => "matroska",
        "video/webm" => "webm",
        "video/quicktime" | "video/mp4" | "audio/x-m4a" => "mp4",
        "video/mpegts" => "mpegts",
        "video/x-msvideo" => "avi",
        "video/mpeg" => "mpeg",
        "video/x-flv" => "flv",
        "application/ogg" => "ogg",
        other => other,
    }
    .into()
}

/// Two-letter codes (what GStreamer normalises to) back to the three-letter
/// ISO 639-2 codes callers filter on. A small table
/// rather than libgsttag: these are the languages releases ship with.
fn iso639_2(code: &str) -> String {
    let three = match code {
        "en" => "eng",
        "nl" => "dut",
        "de" => "ger",
        "fr" => "fre",
        "es" => "spa",
        "it" => "ita",
        "pt" => "por",
        "ru" => "rus",
        "ja" => "jpn",
        "zh" => "chi",
        "ko" => "kor",
        "ar" => "ara",
        "pl" => "pol",
        "sv" => "swe",
        "da" => "dan",
        "no" | "nb" => "nor",
        "fi" => "fin",
        "tr" => "tur",
        "cs" => "cze",
        "hu" => "hun",
        "el" => "gre",
        "he" => "heb",
        "hi" => "hin",
        "th" => "tha",
        "ro" => "rum",
        "uk" => "ukr",
        "vi" => "vie",
        "id" => "ind",
        "ms" => "may",
        "bg" => "bul",
        "hr" => "hrv",
        "sr" => "srp",
        "sk" => "slo",
        "sl" => "slv",
        "et" => "est",
        "lv" => "lav",
        "lt" => "lit",
        "fa" => "per",
        "ca" => "cat",
        "eu" => "baq",
        "gl" => "glg",
        "is" => "ice",
        other => other,
    };
    three.into()
}

fn chapters(toc: &gst::Toc, duration: Option<Duration>) -> Vec<Chapter> {
    fn collect(entries: &[gst::TocEntry], out: &mut Vec<Chapter>) {
        for entry in entries {
            if entry.entry_type() == gst::TocEntryType::Chapter
                && let Some((start, stop)) = entry.start_stop_times()
                && start >= 0
            {
                let title = entry
                    .tags()
                    .and_then(|t| t.get::<gst::tags::Title>().map(|v| v.get().to_string()))
                    .filter(|t| !t.trim().is_empty());
                out.push(Chapter {
                    start: Duration::from_nanos(start as u64),
                    end: Duration::from_nanos(stop.max(start) as u64),
                    title,
                });
            }
            collect(&entry.sub_entries(), out);
        }
    }

    let mut out = Vec::new();
    collect(&toc.entries(), &mut out);
    out.sort_by_key(|c| c.start);
    // Some muxers leave the end open; close each chapter at the next one.
    for i in 0..out.len() {
        if out[i].end <= out[i].start {
            out[i].end = out
                .get(i + 1)
                .map(|n| n.start)
                .or(duration)
                .unwrap_or(out[i].start);
        }
    }
    out
}
