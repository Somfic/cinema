//! Building blocks shared by the pipelines: element construction, codec
//! naming, encoder selection, and the pad probes that turn a demuxed stream
//! into cleanly split fragments.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use gst::prelude::*;

use crate::{Error, Result};

pub(crate) fn make(factory: &str) -> Result<gst::Element> {
    gst::ElementFactory::make(factory)
        .build()
        .map_err(|_| Error::MissingElement(factory.into()))
}

pub(crate) fn exists(factory: &str) -> bool {
    gst::ElementFactory::find(factory).is_some()
}

pub(crate) fn capsfilter(caps: &str) -> Result<gst::Element> {
    let caps: gst::Caps = caps
        .parse()
        .map_err(|_| Error::Pipeline(format!("bad caps `{caps}`")))?;
    gst::ElementFactory::make("capsfilter")
        .property("caps", caps)
        .build()
        .map_err(|_| Error::MissingElement("capsfilter".into()))
}

/// Sets a property only when the element has it. Encoders from different
/// plugins (and plugin versions) disagree on names; a tuning knob that is
/// missing is never worth failing a stream over.
pub(crate) fn try_set(element: &gst::Element, name: &str, value: &str) -> bool {
    let Some(pspec) = element.find_property(name) else {
        return false;
    };
    // Validated first: an enum value one plugin version lacks would
    // otherwise panic.
    match gst::glib::Value::deserialize_with_pspec(value, &pspec) {
        Ok(value) => {
            element.set_property_from_value(name, &value);
            true
        }
        Err(_) => false,
    }
}

/// Sets the first of `values` the element accepts.
fn try_set_first(element: &gst::Element, name: &str, values: &[&str]) {
    for value in values {
        if try_set(element, name, value) {
            return;
        }
    }
}

/// Adds `elements` to `bin`, links them in order and brings them up to the
/// bin's state. Returns the first element, for the caller to link into.
pub(crate) fn add_chain(bin: &gst::Pipeline, elements: &[gst::Element]) -> Result<()> {
    bin.add_many(elements)?;
    gst::Element::link_many(elements)?;
    for element in elements {
        element.sync_state_with_parent()?;
    }
    Ok(())
}

/// Waits for the first bus message `pick` accepts, failing on an error
/// message or after `timeout`.
pub(crate) async fn wait_bus<T>(
    bus: &gst::Bus,
    timeout: Duration,
    what: &'static str,
    mut pick: impl FnMut(&gst::Message) -> Option<T>,
) -> Result<T> {
    let mut messages = bus.stream();
    let wait = async {
        while let Some(msg) = messages.next().await {
            if let gst::MessageView::Error(err) = msg.view() {
                return Err(error_message(err));
            }
            if let Some(v) = pick(&msg) {
                return Ok(v);
            }
        }
        Err(Error::Pipeline("bus closed".into()))
    };
    tokio::time::timeout(timeout, wait)
        .await
        .map_err(|_| Error::Timeout(what))?
}

pub(crate) fn error_message(err: &gst::message::Error) -> Error {
    let source = err
        .src()
        .map(|s| s.path_string().to_string())
        .unwrap_or_default();
    match err.debug() {
        Some(debug) => Error::Pipeline(format!("{source}: {} ({debug})", err.error())),
        None => Error::Pipeline(format!("{source}: {}", err.error())),
    }
}

/// Tears a pipeline down without blocking an async task on it: a state change
/// to NULL waits for streaming threads, which can sit in a torrent read until
/// `unlock` reaches them.
pub(crate) fn shutdown(pipeline: gst::Pipeline) {
    std::thread::spawn(move || {
        let _ = pipeline.set_state(gst::State::Null);
    });
}

/// Conventional short codec name for a stream's caps (`h264`, `hevc`,
/// `eac3`, `subrip`, ...): the names clients report capabilities in.
pub(crate) fn codec_name(caps: &gst::CapsRef) -> String {
    let Some(s) = caps.structure(0) else {
        return "unknown".into();
    };
    let version = |field: &str| s.get::<i32>(field).ok();
    let name = match s.name().as_str() {
        "video/x-h264" => "h264",
        "video/x-h265" => "hevc",
        "video/x-av1" => "av1",
        "video/x-vp9" => "vp9",
        "video/x-vp8" => "vp8",
        "video/x-theora" => "theora",
        "video/x-divx" | "video/x-xvid" => "mpeg4",
        "video/x-wmv" => "wmv",
        "video/mpeg" => match version("mpegversion") {
            Some(1) => "mpeg1video",
            Some(2) => "mpeg2video",
            _ => "mpeg4",
        },
        "audio/mpeg" => match (version("mpegversion"), version("layer")) {
            (Some(1), Some(3)) => "mp3",
            (Some(1), _) => "mp2",
            _ => "aac",
        },
        "audio/x-ac3" => "ac3",
        "audio/x-eac3" => "eac3",
        "audio/x-dts" => "dts",
        "audio/x-true-hd" => "truehd",
        "audio/x-opus" => "opus",
        "audio/x-vorbis" => "vorbis",
        "audio/x-flac" => "flac",
        "audio/x-alac" => "alac",
        "audio/x-wma" => "wma",
        "audio/x-raw" => "pcm",
        "text/x-raw" => "subrip",
        "application/x-ssa" => "ssa",
        "application/x-ass" => "ass",
        "application/x-subtitle-vtt" => "webvtt",
        "subpicture/x-pgs" => "hdmv_pgs_subtitle",
        "subpicture/x-dvd" => "dvd_subtitle",
        "subpicture/x-dvb" => "dvb_subtitle",
        other => {
            return other
                .rsplit('/')
                .next()
                .unwrap_or(other)
                .trim_start_matches("x-")
                .into();
        }
    };
    name.into()
}

/// Subtitle formats whose cues are plain text we can hand to a player.
pub(crate) fn is_text_subtitle(caps: &gst::CapsRef) -> bool {
    caps.structure(0).is_some_and(|s| {
        matches!(
            s.name().as_str(),
            "text/x-raw" | "application/x-ssa" | "application/x-ass" | "application/x-subtitle-vtt"
        )
    })
}

// ── Encoders ──

/// Which hardware family to prefer for video encoding. Parsed from config;
/// unknown values behave like `Auto`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Hardware {
    #[default]
    Auto,
    /// Software (x264) only.
    None,
    Nvidia,
    VaApi,
    VideoToolbox,
}

impl std::str::FromStr for Hardware {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s.to_ascii_lowercase().as_str() {
            "none" | "software" | "cpu" => Hardware::None,
            "nvidia" | "nvenc" | "cuda" => Hardware::Nvidia,
            "vaapi" | "va" | "intel" | "amd" => Hardware::VaApi,
            "videotoolbox" | "vt" | "apple" => Hardware::VideoToolbox,
            _ => Hardware::Auto,
        })
    }
}

/// Video encoder tuning shared by live and background transcodes.
#[derive(Clone, Debug)]
pub struct EncoderSettings {
    pub hardware: Hardware,
    /// x264 speed preset (`ultrafast` … `veryslow`).
    pub preset: String,
    /// Constant-quality target on the CRF scale (lower is better).
    pub crf: u8,
    /// Output height cap; taller sources are scaled down. Unlimited by
    /// default: re-encoded video keeps the source resolution.
    pub max_height: u32,
}

impl Default for EncoderSettings {
    fn default() -> Self {
        Self {
            hardware: Hardware::Auto,
            preset: "veryfast".into(),
            crf: 18,
            max_height: u32::MAX,
        }
    }
}

const H264_ENCODERS: &[(&str, Hardware)] = &[
    ("vtenc_h264_hw", Hardware::VideoToolbox),
    ("vtenc_h264", Hardware::VideoToolbox),
    ("nvh264enc", Hardware::Nvidia),
    ("nvcudah264enc", Hardware::Nvidia),
    ("vah264enc", Hardware::VaApi),
    ("vah264lpenc", Hardware::VaApi),
    ("x264enc", Hardware::None),
];

static WORKING: Mutex<Option<HashMap<&'static str, bool>>> = Mutex::new(None);

/// Whether an encoder actually works here: a plugin can be installed with
/// no device behind it (nvenc on a machine without an NVIDIA GPU). Checked
/// once with a two-frame test encode and cached.
fn encoder_works(factory: &'static str) -> bool {
    if let Some(known) = WORKING
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|m| m.get(factory).copied())
    {
        return known;
    }
    let works = exists(factory)
        && gst::parse::launch(&format!(
            "videotestsrc num-buffers=2 ! video/x-raw,format=NV12,width=320,height=240 ! {factory} ! fakesink"
        ))
        .ok()
        .and_then(|p| p.downcast::<gst::Pipeline>().ok())
        .is_some_and(|p| {
            let ok = p.set_state(gst::State::Playing).is_ok()
                && p.bus().is_some_and(|bus| {
                    bus.timed_pop_filtered(
                        gst::ClockTime::from_seconds(10),
                        &[gst::MessageType::Eos, gst::MessageType::Error],
                    )
                    .is_some_and(|m| m.type_() == gst::MessageType::Eos)
                });
            let _ = p.set_state(gst::State::Null);
            ok
        });
    if works {
        tracing::info!(encoder = factory, "Video encoder available");
    }
    WORKING
        .lock()
        .unwrap()
        .get_or_insert_with(HashMap::new)
        .insert(factory, works);
    works
}

/// Name of the H.264 encoder `settings` resolves to. Blocking: the first call
/// runs a test encode per candidate, so call it once at startup to keep that
/// off the first stream's path.
pub fn h264_encoder_name(settings: &EncoderSettings) -> Option<&'static str> {
    candidates(settings).find(|f| encoder_works(f))
}

fn candidates(settings: &EncoderSettings) -> impl Iterator<Item = &'static str> + '_ {
    H264_ENCODERS
        .iter()
        .filter(|(_, hw)| match settings.hardware {
            Hardware::Auto => true,
            Hardware::None => *hw == Hardware::None,
            pref => *hw == pref || *hw == Hardware::None,
        })
        .map(|(f, _)| *f)
}

/// Bitrate (kbit/s) for encoders that can only target one: generous enough
/// that a big screen shows no blocking, still easy for a home network.
fn bitrate_for(height: u32) -> u32 {
    match height {
        0..=576 => 5_000,
        577..=720 => 9_000,
        721..=1080 => 16_000,
        1081..=1440 => 25_000,
        _ => 40_000,
    }
}

/// Picks the first working H.264 encoder for the preference and tunes it for
/// quality: every encoder runs faster than playback, so nothing is traded
/// for latency. `height` is the output height.
pub(crate) fn h264_encoder(settings: &EncoderSettings, height: u32) -> Result<gst::Element> {
    let factory = h264_encoder_name(settings)
        .ok_or_else(|| Error::MissingElement("an H.264 encoder (x264enc)".into()))?;
    let enc = make(factory)?;
    let crf = settings.crf.to_string();
    let bitrate = bitrate_for(height);
    // Keyframes are forced at segment boundaries; this only caps the gap
    // where a segment runs long.
    let gop = "250";
    match factory {
        "x264enc" => {
            try_set(&enc, "speed-preset", &settings.preset);
            // CRF: constant perceived quality, bits where the picture needs them.
            try_set(&enc, "pass", "qual");
            try_set(&enc, "quantizer", &crf);
            // In quality mode `bitrate` is a ceiling, and its 2 Mbit/s default
            // would starve the CRF; leave room for demanding scenes.
            try_set(&enc, "bitrate", &(bitrate * 2).to_string());
            try_set(&enc, "key-int-max", gop);
        }
        f if f.starts_with("vtenc") => {
            // No constant-quality mode here, so a target bitrate it is.
            try_set(&enc, "realtime", "false");
            try_set(&enc, "allow-frame-reordering", "true");
            try_set(&enc, "rate-control", "abr");
            try_set(&enc, "bitrate", &bitrate.to_string());
            try_set(&enc, "quality", "0.9");
            try_set(&enc, "max-keyframe-interval", gop);
        }
        f if f.starts_with("nv") => {
            // Legacy `nvh264enc` and the newer `nvcudah264enc` name things
            // differently; whichever is present takes its own values.
            try_set_first(&enc, "preset", &["p5", "hq"]);
            try_set(&enc, "tune", "high-quality");
            try_set_first(&enc, "rc-mode", &["vbr"]);
            try_set_first(&enc, "rate-control", &["vbr"]);
            try_set(&enc, "const-quality", &crf);
            try_set(&enc, "bitrate", "0");
            try_set(&enc, "max-bitrate", &(bitrate * 2).to_string());
            try_set(&enc, "bframes", "3");
            try_set(&enc, "gop-size", gop);
        }
        _ => {
            // VA-API: constant QP at the CRF value, best-quality usage.
            try_set(&enc, "rate-control", "cqp");
            try_set(&enc, "qpi", &crf);
            try_set(&enc, "qpp", &crf);
            try_set(&enc, "qpb", &crf);
            try_set(&enc, "target-usage", "1");
            try_set(&enc, "b-frames", "2");
            try_set(&enc, "key-int-max", gop);
        }
    }
    Ok(enc)
}

/// Raw video in, H.264 out: scale down to `max_height`, convert to 8-bit
/// 4:2:0, encode, parse. An HDR source is tone mapped to SDR on the way:
/// 8-bit H.264 can't carry HDR, and skipping it leaves the picture washed
/// out. `source` is the probed video stream.
pub(crate) fn video_encode_chain(
    settings: &EncoderSettings,
    source: Option<&crate::VideoTrack>,
) -> Result<(Vec<gst::Element>, gst::Element)> {
    let mut size = String::new();
    let mut height = source.map(|v| v.height).unwrap_or(1080);
    if let Some(v) = source
        && v.height > settings.max_height
        && v.height > 0
    {
        height = settings.max_height & !1;
        let width = ((v.width as u64 * height as u64 / v.height as u64) as u32) & !1;
        size = format!(",width={width},height={height}");
    }
    let convert = || make("videoconvertscale").or_else(|_| make("videoconvert"));
    let mut chain = vec![make("queue")?, convert()?];
    if source.is_some_and(|v| v.hdr.is_some()) {
        // Scale first (in 10 bits), so the tone mapper sees fewer pixels.
        chain.push(capsfilter(&format!("video/x-raw,format=I420_10LE{size}"))?);
        chain.push(crate::tonemap::ToneMap::element());
        chain.push(convert()?);
        chain.push(capsfilter("video/x-raw,format=NV12")?);
    } else {
        chain.push(capsfilter(&format!("video/x-raw,format=NV12{size}"))?);
    }
    let enc = h264_encoder(settings, height)?;
    chain.push(enc.clone());
    chain.push(make("h264parse")?);
    Ok((chain, enc))
}

/// Raw audio in, stereo AAC out.
pub(crate) fn audio_encode_chain() -> Result<Vec<gst::Element>> {
    let enc = ["fdkaacenc", "avenc_aac", "voaacenc"]
        .iter()
        .find(|f| exists(f))
        .ok_or_else(|| Error::MissingElement("an AAC encoder (fdkaacenc/avenc_aac)".into()))?;
    let enc = make(enc)?;
    try_set(&enc, "bitrate", "192000");
    Ok(vec![
        make("queue")?,
        make("audioconvert")?,
        make("audioresample")?,
        capsfilter("audio/x-raw,channels=2,rate=48000")?,
        enc,
    ])
}

/// Already-encoded video in, MP4-ready video out. Matroska carries no decode
/// timestamps and the MP4 muxers require them, so the timestamper
/// reconstructs them from the frame order.
pub(crate) fn video_copy_chain(codec: &str) -> Result<Vec<gst::Element>> {
    Ok(match codec {
        "h264" => vec![make("queue")?, make("h264parse")?, make("h264timestamper")?],
        "hevc" => vec![
            make("queue")?,
            make("h265parse")?,
            make("h265timestamper")?,
            // Apple players only accept parameter sets in the sample entry.
            capsfilter("video/x-h265,stream-format=hvc1")?,
        ],
        "av1" => vec![make("queue")?, make("av1parse")?],
        "vp9" => vec![make("queue")?, make("vp9parse")?],
        other => return Err(Error::Unsupported(format!("cannot copy {other} video"))),
    })
}

pub(crate) fn audio_copy_chain(codec: &str) -> Result<Vec<gst::Element>> {
    Ok(match codec {
        "aac" => vec![make("queue")?, make("aacparse")?],
        "ac3" | "eac3" => vec![make("queue")?, make("ac3parse")?],
        "opus" => vec![make("queue")?, make("opusparse")?],
        "flac" => vec![make("queue")?, make("flacparse")?],
        other => return Err(Error::Unsupported(format!("cannot copy {other} audio"))),
    })
}

/// Caps for `decodebin3` that stop it from decoding the streams we copy.
pub(crate) fn decodebin_caps(copy_video: Option<&str>, copy_audio: Option<&str>) -> gst::Caps {
    let mut caps = String::from("video/x-raw(ANY); audio/x-raw(ANY)");
    let video = match copy_video {
        Some("h264") => "video/x-h264",
        Some("hevc") => "video/x-h265",
        Some("av1") => "video/x-av1",
        Some("vp9") => "video/x-vp9",
        _ => "",
    };
    let audio = match copy_audio {
        Some("aac") => "audio/mpeg,mpegversion=(int){2,4}",
        Some("ac3") => "audio/x-ac3",
        Some("eac3") => "audio/x-eac3",
        Some("opus") => "audio/x-opus",
        Some("flac") => "audio/x-flac",
        _ => "",
    };
    for extra in [video, audio] {
        if !extra.is_empty() {
            caps.push_str("; ");
            caps.push_str(extra);
        }
    }
    caps.parse().expect("static caps")
}

/// Answers `decodebin3`'s stream collection with the first video stream and
/// the `audio_index`-th audio stream; nothing else gets demuxed further, let
/// alone decoded. Call from a bus sync handler.
pub(crate) fn select_streams(
    decodebin: &gst::Element,
    collection: &gst::StreamCollection,
    audio_index: usize,
) {
    let mut selected = Vec::new();
    let (mut videos, mut audios) = (0, 0);
    for stream in collection.iter() {
        let Some(id) = stream.stream_id() else {
            continue;
        };
        let kind = stream.stream_type();
        if kind.contains(gst::StreamType::VIDEO) {
            if videos == 0 {
                selected.push(id.to_string());
            }
            videos += 1;
        } else if kind.contains(gst::StreamType::AUDIO) {
            if audios == audio_index {
                selected.push(id.to_string());
            }
            audios += 1;
        }
    }
    decodebin.send_event(gst::event::SelectStreams::new(
        selected.iter().map(String::as_str),
    ));
}

/// A seek that starts a fresh pipeline at `position`. Copied video can only
/// start on a keyframe; re-encoded output starts on the exact frame.
pub(crate) fn start_seek(position: Duration, exact: bool) -> gst::Event {
    // Keyframe times from the index are rounded differently than the
    // demuxer's; aim a hair past the keyframe so snapping back lands on it
    // rather than on the one before.
    let position = if exact {
        position
    } else {
        position + Duration::from_millis(1)
    };
    let flags = gst::SeekFlags::FLUSH
        | if exact {
            gst::SeekFlags::ACCURATE
        } else {
            gst::SeekFlags::KEY_UNIT | gst::SeekFlags::SNAP_BEFORE
        };
    gst::event::Seek::new(
        1.0,
        flags,
        gst::SeekType::Set,
        gst::ClockTime::from_nseconds(position.as_nanos() as u64),
        gst::SeekType::None,
        gst::ClockTime::NONE,
    )
}

/// Holds back everything on `pad` - buffers, flushes, segments - until the
/// segment produced by the seek with `seqnum` arrives. The muxers can't take
/// a flush once they have seen data, so they must only ever see the stream
/// from its real start position.
pub(crate) fn gate_until_seek(pad: &gst::Pad, seqnum: gst::Seqnum) {
    let open = Arc::new(AtomicBool::new(false));
    pad.add_probe(gst::PadProbeType::DATA_DOWNSTREAM, move |_, info| {
        if open.load(Ordering::Relaxed) {
            return gst::PadProbeReturn::Remove;
        }
        match &info.data {
            Some(gst::PadProbeData::Event(ev)) => match ev.view() {
                gst::EventView::Segment(_) if ev.seqnum() == seqnum => {
                    open.store(true, Ordering::Relaxed);
                    gst::PadProbeReturn::Ok
                }
                gst::EventView::FlushStart(_)
                | gst::EventView::FlushStop(_)
                | gst::EventView::Segment(_) => gst::PadProbeReturn::Drop,
                _ => gst::PadProbeReturn::Ok,
            },
            Some(gst::PadProbeData::Buffer(_)) | Some(gst::PadProbeData::BufferList(_)) => {
                gst::PadProbeReturn::Drop
            }
            _ => gst::PadProbeReturn::Ok,
        }
    });
}

/// Drops buffers that start before the current segment. A seek can hand
/// over the GOP preceding its target (for decoders to reference); copied
/// streams have no decoder to clip it, and the muxer would fold it into the
/// first fragment - which then starts a segment early.
pub(crate) fn clip_to_segment(pad: &gst::Pad) {
    let segment: Mutex<Option<gst::FormattedSegment<gst::ClockTime>>> = Mutex::new(None);
    pad.add_probe(
        gst::PadProbeType::BUFFER | gst::PadProbeType::EVENT_DOWNSTREAM,
        move |_, info| match &info.data {
            Some(gst::PadProbeData::Event(ev)) => {
                if let gst::EventView::Segment(s) = ev.view() {
                    *segment.lock().unwrap() =
                        s.segment().clone().downcast::<gst::ClockTime>().ok();
                }
                gst::PadProbeReturn::Ok
            }
            Some(gst::PadProbeData::Buffer(buf)) => {
                let before = segment
                    .lock()
                    .unwrap()
                    .as_ref()
                    .zip(buf.pts())
                    .is_some_and(|(s, pts)| s.start().is_some_and(|start| pts < start));
                if before {
                    gst::PadProbeReturn::Drop
                } else {
                    gst::PadProbeReturn::Ok
                }
            }
            _ => gst::PadProbeReturn::Ok,
        },
    );
}

/// Rewrites segments on `pad` so running time equals stream time - the
/// position in the file. The muxer stamps fragments with running time, and a
/// fragment of the file's 10th minute has to say "10 minutes" no matter where
/// the pipeline started.
pub(crate) fn absolute_running_time(pad: &gst::Pad) {
    pad.add_probe(gst::PadProbeType::EVENT_DOWNSTREAM, |_, info| {
        let Some(gst::PadProbeData::Event(ev)) = &info.data else {
            return gst::PadProbeReturn::Ok;
        };
        if let gst::EventView::Segment(s) = ev.view()
            && let Ok(mut segment) = s.segment().clone().downcast::<gst::ClockTime>()
        {
            segment.set_base(segment.time().unwrap_or(gst::ClockTime::ZERO));
            let seqnum = ev.seqnum();
            info.data = Some(gst::PadProbeData::Event(
                gst::event::Segment::builder(&segment)
                    .seqnum(seqnum)
                    .build(),
            ));
        }
        gst::PadProbeReturn::Ok
    });
}

/// Within this much of a boundary counts as on it: container timestamps are
/// rounded (Matroska to the millisecond), and a re-encoded frame lands up to
/// a frame after the boundary it was forced for.
const BOUNDARY_SLACK: u64 = 2_000_000;

/// Tracks stream time on a pad and reports each time a buffer crosses the
/// next segment boundary. Shared by the keyframe forcer and the splitter so
/// both agree on exactly which frame starts a segment.
struct BoundaryTracker {
    boundaries: Arc<[u64]>,
    segment: Option<gst::FormattedSegment<gst::ClockTime>>,
    /// Index into `boundaries` of the next boundary to cross.
    next: usize,
}

impl BoundaryTracker {
    fn new(boundaries: Arc<[u64]>) -> Self {
        Self {
            boundaries,
            segment: None,
            next: usize::MAX,
        }
    }

    fn on_event(&mut self, event: &gst::EventRef) {
        if let gst::EventView::Segment(s) = event.view() {
            self.segment = s.segment().clone().downcast::<gst::ClockTime>().ok();
            self.next = usize::MAX;
        }
    }

    /// Stream time of `buffer` if it is the first at or past the next
    /// boundary (and a keyframe, when `need_key`).
    fn crosses(&mut self, buffer: &gst::BufferRef, need_key: bool) -> bool {
        let Some(segment) = &self.segment else {
            return false;
        };
        let Some(t) = buffer.pts().and_then(|pts| segment.to_stream_time(pts)) else {
            return false;
        };
        let t = t.nseconds();
        if self.next == usize::MAX {
            // The first buffer after a (re)start begins a segment by
            // definition; aim at the boundary after it.
            self.next = self
                .boundaries
                .partition_point(|&b| b <= t + BOUNDARY_SLACK);
            return false;
        }
        let Some(&boundary) = self.boundaries.get(self.next) else {
            return false;
        };
        if t + BOUNDARY_SLACK < boundary {
            return false;
        }
        if need_key && buffer.flags().contains(gst::BufferFlags::DELTA_UNIT) {
            return false;
        }
        self.next = self
            .boundaries
            .partition_point(|&b| b <= t + BOUNDARY_SLACK);
        true
    }
}

/// Asks the encoder behind `sink_pad` for a keyframe on the first frame of
/// every segment.
pub(crate) fn force_keyframes(sink_pad: &gst::Pad, boundaries: Arc<[u64]>) {
    let tracker = Mutex::new(BoundaryTracker::new(boundaries));
    sink_pad.add_probe(
        gst::PadProbeType::BUFFER | gst::PadProbeType::EVENT_DOWNSTREAM,
        move |pad, info| {
            match &info.data {
                Some(gst::PadProbeData::Event(ev)) => tracker.lock().unwrap().on_event(ev),
                Some(gst::PadProbeData::Buffer(buf)) => {
                    let mut tracker = tracker.lock().unwrap();
                    if tracker.crosses(buf, false) {
                        let running_time = tracker
                            .segment
                            .as_ref()
                            .and_then(|s| buf.pts().and_then(|pts| s.to_running_time(pts)));
                        drop(tracker);
                        let event = gst_video::DownstreamForceKeyUnitEvent::builder()
                            .running_time(running_time)
                            .all_headers(true)
                            .build();
                        pad.send_event(event);
                    }
                }
                _ => {}
            }
            gst::PadProbeReturn::Ok
        },
    );
}

/// Makes `cmafmux` (in manual-split mode) start a new fragment exactly at
/// each segment boundary, on the keyframe for video. `src_pad` is the pad
/// feeding the muxer.
pub(crate) fn split_at_boundaries(src_pad: &gst::Pad, boundaries: Arc<[u64]>, video: bool) {
    let tracker = Mutex::new(BoundaryTracker::new(boundaries));
    src_pad.add_probe(
        gst::PadProbeType::BUFFER | gst::PadProbeType::EVENT_DOWNSTREAM,
        move |pad, info| {
            match &info.data {
                Some(gst::PadProbeData::Event(ev)) => tracker.lock().unwrap().on_event(ev),
                Some(gst::PadProbeData::Buffer(buf))
                    if tracker.lock().unwrap().crosses(buf, video) =>
                {
                    let split = gst::event::CustomDownstream::builder(
                        gst::Structure::builder("FMP4MuxSplitNow")
                            .field("chunk", false)
                            .build(),
                    )
                    .build();
                    pad.push_event(split);
                }
                _ => {}
            }
            gst::PadProbeReturn::Ok
        },
    );
}
