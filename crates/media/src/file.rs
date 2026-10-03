//! Background transcodes to a plain MP4 that browsers and Chromecasts play
//! directly, with native seeking and no packaging at play time.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gst::prelude::*;

use crate::pipeline::{self, EncoderSettings};
use crate::{AudioAction, Error, Input, MediaInfo, Result, VideoAction};

pub struct TranscodeOptions {
    pub input: Input,
    pub info: Arc<MediaInfo>,
    pub video: VideoAction,
    pub audio: AudioAction,
    pub audio_index: usize,
    pub encoder: EncoderSettings,
    /// Final location. Written as `<output>.part` and renamed on success.
    pub output: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Status {
    Running,
    Done,
    Failed(String),
}

/// A running transcode. Dropping it cancels the job and removes the partial
/// output.
pub struct FileTranscode {
    pipeline: gst::Pipeline,
    /// The source's output, blocked while paused.
    source_pad: gst::Pad,
    paused: Mutex<Option<gst::PadProbeId>>,
    status: tokio::sync::watch::Receiver<Status>,
    part: PathBuf,
    output: PathBuf,
    finished: Mutex<bool>,
}

impl FileTranscode {
    pub fn start(opts: TranscodeOptions) -> Result<Self> {
        crate::init()?;
        if let Some(dir) = opts.output.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let part = opts.output.with_extension("mp4.part");
        let _ = std::fs::remove_file(&part);

        let pipeline = gst::Pipeline::new();
        let src = opts.input.source_element()?;
        let decodebin = pipeline::make("decodebin3")?;
        let mux = pipeline::make("mp4mux")?;
        // Index at the front, so players can start before the end has loaded.
        mux.set_property("faststart", true);
        mux.set_property(
            "faststart-file",
            opts.output
                .with_extension("mp4.moov")
                .to_string_lossy()
                .as_ref(),
        );
        let sink = gst::ElementFactory::make("filesink")
            .property("location", part.to_string_lossy().as_ref())
            .property("sync", false)
            .build()
            .map_err(|_| Error::MissingElement("filesink".into()))?;

        let video_codec = opts.info.video.first().map(|v| v.codec.clone());
        let audio = opts.info.audio.get(opts.audio_index).cloned();
        decodebin.set_property(
            "caps",
            pipeline::decodebin_caps(
                (opts.video == VideoAction::Copy)
                    .then_some(video_codec.as_deref())
                    .flatten(),
                (opts.audio == AudioAction::Copy)
                    .then_some(audio.as_ref().map(|a| a.codec.as_str()))
                    .flatten(),
            ),
        );

        pipeline.add_many([&src, &decodebin, &mux, &sink])?;
        src.link(&decodebin)?;
        mux.link(&sink)?;
        // The muxer takes no new inputs once it has started, so its pads are
        // requested before any stream turns up.
        let mux_video = request_pad(&mux, "video_%u")?;
        let mux_audio = match opts.audio {
            AudioAction::None => None,
            _ if opts.info.audio.is_empty() => None,
            _ => Some(request_pad(&mux, "audio_%u")?),
        };

        let (status_tx, status) = tokio::sync::watch::channel(Status::Running);
        let status_tx = Arc::new(status_tx);
        let fail = {
            let status_tx = status_tx.clone();
            move |msg: String| {
                status_tx.send_if_modified(|s| {
                    if *s == Status::Running {
                        *s = Status::Failed(msg);
                        true
                    } else {
                        false
                    }
                });
            }
        };

        let bus = pipeline.bus().expect("pipeline bus");
        let decodebin_bus = decodebin.downgrade();
        let audio_index = opts.audio_index;
        let fail_bus = fail.clone();
        let status_bus = status_tx.clone();
        bus.set_sync_handler(move |_, msg| {
            match msg.view() {
                gst::MessageView::StreamCollection(sc) => {
                    if let Some(decodebin) = decodebin_bus.upgrade() {
                        pipeline::select_streams(&decodebin, &sc.stream_collection(), audio_index);
                    }
                }
                gst::MessageView::Eos(_) => {
                    status_bus.send_replace(Status::Done);
                }
                gst::MessageView::Error(err) => fail_bus(pipeline::error_message(err).to_string()),
                _ => {}
            }
            gst::BusSyncReply::Drop
        });

        let pipeline_weak = pipeline.downgrade();
        let info = opts.info.clone();
        let encoder = opts.encoder.clone();
        let (video, audio_action) = (opts.video, opts.audio);
        decodebin.connect_pad_added(move |_, pad| {
            let Some(pipeline) = pipeline_weak.upgrade() else {
                return;
            };
            let (chain, target) = if pad.name().starts_with("video") {
                let chain = match video {
                    VideoAction::Copy => pipeline::video_copy_chain(
                        info.video.first().map(|v| v.codec.as_str()).unwrap_or(""),
                    ),
                    VideoAction::Transcode => {
                        { pipeline::video_encode_chain(&encoder, info.video.first()) }
                            .map(|(chain, _)| chain)
                    }
                };
                (chain, mux_video.clone())
            } else if pad.name().starts_with("audio")
                && let Some(mux_audio) = &mux_audio
            {
                let chain = match audio_action {
                    AudioAction::Copy => pipeline::audio_copy_chain(
                        audio.as_ref().map(|a| a.codec.as_str()).unwrap_or(""),
                    ),
                    _ => pipeline::audio_encode_chain(),
                };
                (chain, mux_audio.clone())
            } else {
                return;
            };
            let linked = chain.and_then(|chain| {
                pipeline::add_chain(&pipeline, &chain)?;
                chain
                    .last()
                    .unwrap()
                    .static_pad("src")
                    .unwrap()
                    .link(&target)
                    .map_err(|e| Error::Pipeline(format!("{e:?}")))?;
                pad.link(&chain[0].static_pad("sink").unwrap())
                    .map_err(|e| Error::Pipeline(format!("{e:?}")))?;
                Ok(())
            });
            if let Err(err) = linked {
                fail(err.to_string());
            }
        });

        pipeline.set_state(gst::State::Playing)?;
        Ok(Self {
            pipeline,
            source_pad: src.static_pad("src").expect("source pad"),
            paused: Mutex::new(None),
            status,
            part,
            output: opts.output,
            finished: Mutex::new(false),
        })
    }

    /// How far into the file the transcode has got.
    pub fn position(&self) -> Option<Duration> {
        self.pipeline
            .query_position::<gst::ClockTime>()
            .map(|p| Duration::from_nanos(p.nseconds()))
    }

    /// Suspends the job in place; [`resume`](Self::resume) continues it.
    ///
    /// Blocks the source rather than pausing the pipeline: with faststart
    /// the muxer writes media to its own scratch file, so a paused sink
    /// would hold nothing back.
    pub fn pause(&self) {
        let mut paused = self.paused.lock().unwrap();
        if paused.is_none() {
            *paused = self
                .source_pad
                .add_probe(gst::PadProbeType::BLOCK_DOWNSTREAM, |_, _| {
                    gst::PadProbeReturn::Ok
                });
        }
    }

    pub fn resume(&self) {
        if let Some(id) = self.paused.lock().unwrap().take() {
            self.source_pad.remove_probe(id);
        }
    }

    /// Resolves when the transcode finishes: the output is in place on `Ok`.
    /// Cancel-safe; call again after dropping the future.
    pub async fn wait(&self) -> Result<()> {
        let mut status = self.status.clone();
        let status = status
            .wait_for(|s| *s != Status::Running)
            .await
            .map_err(|_| Error::Cancelled)?
            .clone();
        let pipeline = self.pipeline.clone();
        // Waits for the muxer to rewrite the header; off the async runtime.
        tokio::task::spawn_blocking(move || pipeline.set_state(gst::State::Null))
            .await
            .map_err(|_| Error::Cancelled)??;
        match status {
            Status::Done => {
                tokio::fs::rename(&self.part, &self.output).await?;
                *self.finished.lock().unwrap() = true;
                Ok(())
            }
            Status::Failed(err) => Err(Error::Pipeline(err)),
            Status::Running => unreachable!(),
        }
    }
}

impl Drop for FileTranscode {
    fn drop(&mut self) {
        if *self.finished.lock().unwrap() {
            return;
        }
        let pipeline = self.pipeline.clone();
        let part = self.part.clone();
        let moov = self.output.with_extension("mp4.moov");
        std::thread::spawn(move || {
            let _ = pipeline.set_state(gst::State::Null);
            let _ = std::fs::remove_file(part);
            let _ = std::fs::remove_file(moov);
        });
    }
}

/// Muxes the video of `video` and the audio of `audio` (or of `video`, when
/// it has its own) into a faststart MP4 at `output`, without re-encoding.
pub async fn mux_mp4(
    video: &std::path::Path,
    audio: Option<&std::path::Path>,
    output: &std::path::Path,
) -> Result<()> {
    crate::init()?;
    let pipeline = gst::Pipeline::new();
    let result = mux(&pipeline, video, audio, output).await;
    let _ = tokio::task::spawn_blocking({
        let pipeline = pipeline.clone();
        move || pipeline.set_state(gst::State::Null)
    })
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(output).await;
    }
    result
}

async fn mux(
    pipeline: &gst::Pipeline,
    video: &std::path::Path,
    audio: Option<&std::path::Path>,
    output: &std::path::Path,
) -> Result<()> {
    // Which file each stream comes from, decided before anything plays: the
    // muxer takes no new inputs once it has started.
    let video_info = crate::probe(&Input::File(video.to_owned())).await?;
    let video_codec = video_info
        .video
        .first()
        .map(|v| v.codec.clone())
        .ok_or_else(|| Error::Unsupported("no video stream".into()))?;
    let audio_source = match audio {
        Some(path) => {
            let info = crate::probe(&Input::File(path.to_owned())).await?;
            info.audio.first().map(|a| (path, a.codec.clone()))
        }
        None => video_info.audio.first().map(|a| (video, a.codec.clone())),
    };

    let mux = pipeline::make("mp4mux")?;
    mux.set_property("faststart", true);
    mux.set_property(
        "faststart-file",
        output.with_extension("moov").to_string_lossy().as_ref(),
    );
    let sink = gst::ElementFactory::make("filesink")
        .property("location", output.to_string_lossy().as_ref())
        .build()
        .map_err(|_| Error::MissingElement("filesink".into()))?;
    pipeline.add_many([&mux, &sink])?;
    mux.link(&sink)?;

    let mut wanted = vec![(
        video,
        true,
        pipeline::video_copy_chain(&video_codec)?,
        request_pad(&mux, "video_%u")?,
    )];
    if let Some((path, codec)) = audio_source {
        wanted.push((
            path,
            false,
            pipeline::audio_copy_chain(&codec)?,
            request_pad(&mux, "audio_%u")?,
        ));
    }

    let failure: Arc<Mutex<Option<String>>> = Default::default();
    let mut sources: Vec<(&std::path::Path, gst::Element)> = Vec::new();
    for (path, is_video, chain, target) in wanted {
        pipeline::add_chain(pipeline, &chain)?;
        chain
            .last()
            .unwrap()
            .static_pad("src")
            .unwrap()
            .link(&target)
            .map_err(|e| Error::Pipeline(format!("{e:?}")))?;
        let entry = chain[0].static_pad("sink").unwrap();

        // One demuxer per file, shared when both streams come from it.
        let parsebin = match sources.iter().find(|(p, _)| *p == path) {
            Some((_, parsebin)) => parsebin.clone(),
            None => {
                let src = Input::File(path.to_owned()).source_element()?;
                let parsebin = pipeline::make("parsebin")?;
                pipeline.add_many([&src, &parsebin])?;
                src.link(&parsebin)?;
                sources.push((path, parsebin.clone()));
                parsebin
            }
        };
        let failure = failure.clone();
        let pipeline_weak = pipeline.downgrade();
        let taken = Arc::new(Mutex::new(false));
        parsebin.connect_pad_added(move |_, pad| {
            let caps = pad.current_caps().unwrap_or_else(|| pad.query_caps(None));
            let kind = caps
                .structure(0)
                .map(|s| s.name().to_string())
                .unwrap_or_default();
            let ours = kind.starts_with(if is_video { "video/" } else { "audio/" });
            let mut taken = taken.lock().unwrap();
            if ours && !*taken && !entry.is_linked() {
                *taken = true;
                if let Err(err) = pad.link(&entry) {
                    *failure.lock().unwrap() = Some(format!("{kind}: {err:?}"));
                }
                return;
            }
            drop(taken);
            // Everything else is drained, unless another branch claims it.
            if pad.is_linked() || is_video == kind.starts_with("audio/") {
                return;
            }
            if let Some(pipeline) = pipeline_weak.upgrade()
                && let Ok(fake) = gst::ElementFactory::make("fakesink")
                    .property("sync", false)
                    .property("async", false)
                    .build()
                && pipeline.add(&fake).is_ok()
            {
                let _ = fake.sync_state_with_parent();
                let _ = pad.link(&fake.static_pad("sink").unwrap());
            }
        });
    }

    let bus = pipeline.bus().expect("pipeline bus");
    pipeline.set_state(gst::State::Playing)?;
    let result = pipeline::wait_bus(&bus, Duration::from_secs(120), "muxing", |msg| {
        matches!(msg.view(), gst::MessageView::Eos(_)).then_some(())
    })
    .await;
    match failure.lock().unwrap().take() {
        Some(err) => Err(Error::Pipeline(err)),
        None => result,
    }
}

fn request_pad(mux: &gst::Element, template: &str) -> Result<gst::Pad> {
    mux.request_pad_simple(template)
        .ok_or_else(|| Error::Pipeline(format!("muxer has no {template} pad")))
}
