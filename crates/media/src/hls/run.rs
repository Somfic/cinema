//! One packaging run: a pipeline that starts at a segment boundary and
//! produces consecutive segments from there until it reaches the end, falls
//! too far ahead of the player, or is replaced by a run elsewhere.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use gst::prelude::*;

use super::Track;
use crate::pipeline::{self, EncoderSettings};
use crate::{AudioAction, Input, MediaInfo, Result, VideoAction};

/// What a run needs to build its pipeline.
pub(super) struct RunSpec {
    pub input: Input,
    pub info: Arc<MediaInfo>,
    pub video: VideoAction,
    pub audio: AudioAction,
    pub audio_index: usize,
    pub encoder: EncoderSettings,
    pub boundaries: Arc<[u64]>,
    pub start: usize,
}

/// A finished piece of output from a run.
pub(super) enum Output {
    Init(Track, bytes::Bytes),
    /// A fragment that starts at stream time `pts` (ns).
    Fragment(Track, u64, bytes::Bytes),
    Eos(Track),
    Error(String),
}

/// Backpressure between the session and a run's streaming threads. A run
/// blocks once it is `ahead` segments past the furthest one requested, so a
/// paused player doesn't get the rest of the film encoded behind its back.
pub(super) struct Throttle {
    state: Mutex<ThrottleState>,
    cv: Condvar,
}

struct ThrottleState {
    wanted: usize,
    stopped: bool,
}

/// How far a run may get ahead of the furthest requested segment.
const AHEAD: usize = 15;

impl Throttle {
    pub fn new(wanted: usize) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(ThrottleState {
                wanted,
                stopped: false,
            }),
            cv: Condvar::new(),
        })
    }

    pub fn want(&self, index: usize) {
        let mut s = self.state.lock().unwrap();
        if index > s.wanted {
            s.wanted = index;
            self.cv.notify_all();
        }
    }

    pub fn stop(&self) {
        self.state.lock().unwrap().stopped = true;
        self.cv.notify_all();
    }

    pub fn stopped(&self) -> bool {
        self.state.lock().unwrap().stopped
    }

    /// Blocks while `next` is too far ahead. False once the run is stopped.
    fn wait_for(&self, next: usize) -> bool {
        let mut s = self.state.lock().unwrap();
        while !s.stopped && next > s.wanted + AHEAD {
            s = self.cv.wait(s).unwrap();
        }
        !s.stopped
    }
}

pub(super) struct Run {
    pub start: usize,
    pub pipeline: gst::Pipeline,
    pub throttle: Arc<Throttle>,
    /// Next segment index each track will produce.
    pub next: [Arc<AtomicUsize>; 2],
    pub finished: [bool; 2],
}

impl Run {
    pub fn next(&self, track: Track) -> usize {
        self.next[track as usize].load(Ordering::Relaxed)
    }

    pub fn stop(self) {
        self.throttle.stop();
        pipeline::shutdown(self.pipeline);
    }
}

pub(super) fn start(spec: RunSpec, output: impl Fn(Output) + Send + Sync + 'static) -> Result<Run> {
    let output = Arc::new(output);
    let pipeline = gst::Pipeline::new();
    let src = spec.input.source_element()?;
    let decodebin = pipeline::make("decodebin3")?;

    let video_codec = spec.info.video.first().map(|v| v.codec.clone());
    let audio_track = spec.info.audio.get(spec.audio_index).cloned();
    let copy_video = (spec.video == VideoAction::Copy)
        .then_some(video_codec.as_deref())
        .flatten();
    let copy_audio = (spec.audio == AudioAction::Copy)
        .then_some(audio_track.as_ref().map(|a| a.codec.as_str()))
        .flatten();
    decodebin.set_property("caps", pipeline::decodebin_caps(copy_video, copy_audio));

    pipeline.add_many([&src, &decodebin])?;
    src.link(&decodebin)?;
    let bus = pipeline.bus().expect("pipeline bus");
    let spec_audio_index = spec.audio_index;

    let throttle = Throttle::new(spec.start);
    let next = [
        Arc::new(AtomicUsize::new(spec.start)),
        Arc::new(AtomicUsize::new(spec.start)),
    ];

    // A run that doesn't begin at the start seeks there first, and holds
    // everything back from the muxers until that seek has taken effect.
    let seek = (spec.start > 0).then(|| {
        pipeline::start_seek(
            Duration::from_nanos(spec.boundaries[spec.start]),
            spec.video == VideoAction::Transcode,
        )
    });
    let expected_pads = 1 + usize::from(spec.audio != AudioAction::None);
    let linked = Arc::new(AtomicUsize::new(0));

    let spec = Arc::new(spec);
    let pipeline_weak = pipeline.downgrade();
    let decodebin_weak = decodebin.downgrade();
    let throttle_cb = throttle.clone();
    let next_cb = next.clone();
    let output_cb = output.clone();
    let seek_cb = seek.clone();
    decodebin.connect_pad_added(move |_, pad| {
        let track = if pad.name().starts_with("video") {
            Track::Video
        } else if pad.name().starts_with("audio") {
            Track::Audio
        } else {
            return;
        };
        let Some(pipeline) = pipeline_weak.upgrade() else {
            return;
        };
        if let Some(stream) = pad.stream() {
            let language = stream.tags().and_then(|t| {
                t.get::<gst::tags::LanguageCode>()
                    .map(|v| v.get().to_string())
            });
            tracing::debug!(?track, id = ?stream.stream_id(), ?language, "Packaging stream");
        }
        let built = branch(
            &pipeline,
            &spec,
            track,
            throttle_cb.clone(),
            next_cb[track as usize].clone(),
            output_cb.clone(),
        );
        let entry = match built {
            Ok(entry) => entry,
            Err(err) => {
                output_cb(Output::Error(err.to_string()));
                return;
            }
        };
        if let Some(seek) = &seek_cb {
            pipeline::gate_until_seek(pad, seek.seqnum());
        }
        if let Err(err) = pad.link(&entry) {
            output_cb(Output::Error(format!("linking {track:?}: {err:?}")));
            return;
        }
        if linked.fetch_add(1, Ordering::SeqCst) + 1 == expected_pads
            && let (Some(seek), Some(decodebin)) = (seek_cb.clone(), decodebin_weak.upgrade())
        {
            // Not from this streaming thread: a flushing seek waits for it.
            std::thread::spawn(move || {
                if !decodebin.send_event(seek) {
                    tracing::warn!("Start seek was not handled");
                }
            });
        }
    });

    // Stream selection, and errors for the session; nothing else listens.
    let output_bus = output.clone();
    let decodebin_bus = decodebin.downgrade();
    let audio_index = spec_audio_index;
    bus.set_sync_handler(move |_, msg| {
        match msg.view() {
            gst::MessageView::StreamCollection(sc) => {
                if let Some(decodebin) = decodebin_bus.upgrade() {
                    pipeline::select_streams(&decodebin, &sc.stream_collection(), audio_index);
                }
            }
            gst::MessageView::Error(err) => {
                output_bus(Output::Error(pipeline::error_message(err).to_string()));
            }
            _ => {}
        }
        gst::BusSyncReply::Drop
    });

    pipeline.set_state(gst::State::Playing)?;

    Ok(Run {
        start: next[0].load(Ordering::Relaxed),
        pipeline,
        throttle,
        next,
        finished: [false; 2],
    })
}

/// Builds the encode/copy → mux → appsink chain for one track and returns
/// the pad to link the decoder output into.
fn branch(
    pipeline: &gst::Pipeline,
    spec: &RunSpec,
    track: Track,
    throttle: Arc<Throttle>,
    next: Arc<AtomicUsize>,
    output: Arc<dyn Fn(Output) + Send + Sync>,
) -> Result<gst::Pad> {
    let mut chain = match track {
        Track::Video => {
            let video = spec.info.video.first();
            match spec.video {
                VideoAction::Copy => {
                    pipeline::video_copy_chain(video.map(|v| v.codec.as_str()).unwrap_or(""))?
                }
                VideoAction::Transcode => {
                    let (chain, encoder) =
                        pipeline::video_encode_chain(&spec.encoder, video, true)?;
                    pipeline::force_keyframes(
                        &encoder.static_pad("sink").expect("encoder sink"),
                        spec.boundaries.clone(),
                    );
                    chain
                }
            }
        }
        Track::Audio => match spec.audio {
            AudioAction::Copy => pipeline::audio_copy_chain(
                spec.info
                    .audio
                    .get(spec.audio_index)
                    .map(|a| a.codec.as_str())
                    .unwrap_or(""),
            )?,
            _ => pipeline::audio_encode_chain()?,
        },
    };

    let mux = gst::ElementFactory::make("cmafmux")
        .property("manual-split", true)
        .build()
        .map_err(|_| crate::Error::MissingElement("cmafmux".into()))?;
    let sink = gst_app::AppSink::builder().sync(false).build();
    sink.set_property("async", false);

    let last = chain.last().expect("non-empty chain").clone();
    pipeline::split_at_boundaries(
        &last.static_pad("src").expect("chain src"),
        spec.boundaries.clone(),
        track == Track::Video,
    );

    let first = chain[0].clone();
    let copied = match track {
        Track::Video => spec.video == VideoAction::Copy,
        Track::Audio => spec.audio == AudioAction::Copy,
    };
    if copied {
        pipeline::clip_to_segment(&first.static_pad("sink").expect("chain sink"));
    }
    chain.push(mux.clone());
    chain.push(sink.clone().upcast());
    pipeline::add_chain(pipeline, &chain)?;
    pipeline::absolute_running_time(&mux.sink_pads()[0]);

    let boundaries = spec.boundaries.clone();
    let output_eos = output.clone();
    let mut pending: Vec<u8> = Vec::new();
    let mut timescale: Option<u32> = None;
    sink.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                if throttle.stopped() {
                    return Err(gst::FlowError::Flushing);
                }
                let buffers: Vec<gst::Buffer> = match sample.buffer_list_owned() {
                    Some(list) => list.iter_owned().collect(),
                    None => sample.buffer_owned().into_iter().collect(),
                };
                for buffer in buffers {
                    let flags = buffer.flags();
                    let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
                    if flags.contains(gst::BufferFlags::HEADER | gst::BufferFlags::DISCONT) {
                        timescale = super::mp4::timescale(&map);
                        output(Output::Init(track, bytes::Bytes::copy_from_slice(&map)));
                        continue;
                    }
                    if flags.contains(gst::BufferFlags::HEADER) {
                        pending.clear();
                    }
                    pending.extend_from_slice(&map);
                    if flags.contains(gst::BufferFlags::MARKER) {
                        // The fragment's own decode time says where it is.
                        let Some(pts) = timescale.and_then(|ts| super::mp4::start_ns(&pending, ts))
                        else {
                            output(Output::Error("fragment without a start time".into()));
                            return Err(gst::FlowError::Error);
                        };
                        let data = bytes::Bytes::from(std::mem::take(&mut pending));
                        output(Output::Fragment(track, pts, data));
                        let produced = super::index_at(&boundaries, pts) + 1;
                        next.store(produced, Ordering::Relaxed);
                        if !throttle.wait_for(produced) {
                            return Err(gst::FlowError::Flushing);
                        }
                    }
                }
                Ok(gst::FlowSuccess::Ok)
            })
            .eos(move |_| output_eos(Output::Eos(track)))
            .build(),
    );

    Ok(first.static_pad("sink").expect("chain sink"))
}
