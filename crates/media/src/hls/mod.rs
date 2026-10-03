//! On-demand HLS.
//!
//! A [`Session`] presents a whole file as a finished VOD playlist from the
//! first request: every segment is listed with its exact duration, so players
//! (a browser's hls.js, a Chromecast) can seek anywhere natively. Segments
//! are produced when asked for. A request near what the current run is
//! producing waits for it; a request elsewhere (a seek) replaces the run with
//! one that starts right there. Produced segments are kept on disk, so
//! seeking back is free.
//!
//! Every fragment is stamped with its absolute position in the file and cut
//! exactly on a segment boundary, so segments from different runs line up
//! seamlessly and audio stays in sync with video across seeks.

mod mp4;
mod run;
mod segments;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;

use crate::pipeline::EncoderSettings;
use crate::{AudioAction, Error, Input, MediaInfo, Result, VideoAction};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Track {
    Video = 0,
    Audio = 1,
}

impl Track {
    fn name(self) -> &'static str {
        match self {
            Track::Video => "video",
            Track::Audio => "audio",
        }
    }
}

pub struct SessionOptions {
    pub input: Input,
    pub info: Arc<MediaInfo>,
    pub video: VideoAction,
    pub audio: AudioAction,
    pub audio_index: usize,
    pub encoder: EncoderSettings,
    /// Scratch directory for produced segments; created, and removed when the
    /// session is dropped.
    pub dir: PathBuf,
    /// Told where each new run starts (a seek, in effect), so a caller
    /// reading from a slow source can fetch that part first.
    pub on_seek: Option<Arc<dyn Fn(Duration) + Send + Sync>>,
}

/// How long a segment request waits for its run before giving up. Long,
/// because a cold torrent may take a while to deliver the pieces.
const SEGMENT_TIMEOUT: Duration = Duration::from_secs(90);

/// A request this many segments past what the current run has produced
/// starts a new run there instead of waiting for it.
const JUMP: usize = 3;

pub struct Session {
    inner: Arc<Inner>,
}

struct Inner {
    opts: SessionOptions,
    boundaries: Arc<[u64]>,
    duration: u64,
    state: Mutex<State>,
    changed: tokio::sync::watch::Sender<u64>,
}

struct State {
    init: [Option<Bytes>; 2],
    done: [Vec<bool>; 2],
    run: Option<run::Run>,
    /// Bumped per run, so output from a replaced run is recognised and
    /// dropped.
    generation: u64,
    error: Option<String>,
}

impl Session {
    pub async fn new(opts: SessionOptions) -> Result<Self> {
        crate::init()?;
        let duration = opts
            .info
            .duration
            .ok_or_else(|| Error::Unsupported("unknown duration".into()))?
            .as_nanos() as u64;
        if opts.info.video.is_empty() {
            return Err(Error::Unsupported("no video stream".into()));
        }

        // Copied video can only split on the file's own keyframes; read
        // where they are so the playlist can be exact. Without an index the
        // playlist is approximate: segments end on the first keyframe past
        // each boundary.
        let keyframes = match opts.video {
            VideoAction::Copy => match crate::keyframes(&opts.input).await {
                Ok(k) => k,
                Err(err) => {
                    tracing::warn!(%err, "Could not read keyframe index");
                    None
                }
            },
            VideoAction::Transcode => None,
        };
        if opts.video == VideoAction::Copy && keyframes.is_none() {
            tracing::info!(input = ?opts.input, "No keyframe index; segment times are approximate");
        }
        let boundaries: Arc<[u64]> = segments::boundaries(duration, keyframes.as_deref()).into();

        tokio::fs::create_dir_all(&opts.dir).await?;
        let n = boundaries.len();
        Ok(Self {
            inner: Arc::new(Inner {
                opts,
                boundaries,
                duration,
                state: Mutex::new(State {
                    init: [None, None],
                    done: [vec![false; n], vec![false; n]],
                    run: None,
                    generation: 0,
                    error: None,
                }),
                changed: tokio::sync::watch::channel(0).0,
            }),
        })
    }

    pub fn segment_count(&self) -> usize {
        self.inner.boundaries.len()
    }

    pub fn duration(&self) -> Duration {
        Duration::from_nanos(self.inner.duration)
    }

    pub fn has_audio(&self) -> bool {
        self.inner.opts.audio != AudioAction::None
    }

    /// The last pipeline error, if packaging failed.
    pub fn error(&self) -> Option<String> {
        self.inner.state.lock().unwrap().error.clone()
    }

    /// Serves a file of the session by its playlist-relative name:
    /// `master.m3u8`, `{video,audio}.m3u8`, `{track}_init.mp4` or
    /// `{track}_{n}.m4s`. Returns the bytes and their content type.
    pub async fn serve(&self, name: &str) -> Result<(Bytes, &'static str)> {
        const PLAYLIST: &str = "application/vnd.apple.mpegurl";
        match name {
            "master.m3u8" => return Ok((self.master_playlist().into(), PLAYLIST)),
            "video.m3u8" => return Ok((self.media_playlist(Track::Video).into(), PLAYLIST)),
            "audio.m3u8" if self.has_audio() => {
                return Ok((self.media_playlist(Track::Audio).into(), PLAYLIST));
            }
            _ => {}
        }
        let not_found = || Error::NotFound(name.to_string());
        let (track, rest) = name.split_once('_').ok_or_else(not_found)?;
        let track = match track {
            "video" => Track::Video,
            "audio" if self.has_audio() => Track::Audio,
            _ => return Err(not_found()),
        };
        if rest == "init.mp4" {
            return Ok((self.init(track).await?, "video/mp4"));
        }
        let index = rest
            .strip_suffix(".m4s")
            .and_then(|n| n.parse::<usize>().ok())
            .ok_or_else(not_found)?;
        Ok((self.segment(track, index).await?, "video/iso.segment"))
    }

    pub fn master_playlist(&self) -> String {
        let info = &self.inner.opts.info;
        let video = &info.video[0];
        let audio = info.audio.get(self.inner.opts.audio_index);
        let (video_codec, resolution, bandwidth) = match self.inner.opts.video {
            VideoAction::Copy => (
                video.mime_codec.as_deref(),
                (video.width > 0).then_some((video.width, video.height)),
                20_000_000,
            ),
            // The encoder's exact profile isn't known before it runs.
            VideoAction::Transcode => (None, None, 8_000_000),
        };
        let audio_codec = match self.inner.opts.audio {
            AudioAction::Copy => audio.and_then(|a| a.mime_codec.as_deref()),
            AudioAction::Transcode => Some("mp4a.40.2"),
            AudioAction::None => None,
        };
        let name = audio
            .and_then(|a| a.title.clone().or_else(|| a.language.clone()))
            .unwrap_or_else(|| "Audio".into());
        segments::master_playlist(&segments::MasterInfo {
            video_codec,
            audio_codec,
            resolution,
            bandwidth,
            audio: self.has_audio().then(|| segments::AudioRendition {
                name: &name,
                language: audio.and_then(|a| a.language.as_deref()),
                channels: match self.inner.opts.audio {
                    AudioAction::Transcode => 2,
                    _ => audio.map(|a| a.channels).unwrap_or(2),
                },
            }),
        })
    }

    pub fn media_playlist(&self, track: Track) -> String {
        segments::media_playlist(track.name(), &self.inner.boundaries, self.inner.duration)
    }

    pub async fn init(&self, track: Track) -> Result<Bytes> {
        // Any run produces the init segment first; start one if needed.
        let mut changed = self.inner.changed.subscribe();
        let mut started: Option<u64> = None;
        let wait = async {
            loop {
                {
                    let mut state = self.inner.state.lock().unwrap();
                    if let Some(init) = &state.init[track as usize] {
                        return Ok(init.clone());
                    }
                    if let Some(err) = state.failed(started) {
                        return Err(err);
                    }
                    if state.run.is_none() {
                        started = Some(self.inner.start_run(&mut state, 0)?);
                    }
                    changed.borrow_and_update();
                }
                if changed.changed().await.is_err() {
                    return Err(Error::Cancelled);
                }
            }
        };
        tokio::time::timeout(SEGMENT_TIMEOUT, wait)
            .await
            .map_err(|_| Error::Timeout("waiting for the init segment"))?
    }

    pub async fn segment(&self, track: Track, index: usize) -> Result<Bytes> {
        if index >= self.inner.boundaries.len() {
            return Err(Error::NotFound(format!("segment {index}")));
        }
        let mut changed = self.inner.changed.subscribe();
        let mut started: Option<u64> = None;
        let wait = async {
            loop {
                {
                    let mut state = self.inner.state.lock().unwrap();
                    if state.done[track as usize][index] {
                        break;
                    }
                    if let Some(err) = state.failed(started) {
                        return Err(err);
                    }
                    let covered = state.run.as_ref().is_some_and(|run| {
                        let next = run.next(track);
                        !run.finished[track as usize]
                            && index >= run.start
                            && index >= next
                            && index <= next + JUMP
                    });
                    match &state.run {
                        Some(run) if covered => run.throttle.want(index),
                        _ => started = Some(self.inner.start_run(&mut state, index)?),
                    }
                    changed.borrow_and_update();
                }
                if changed.changed().await.is_err() {
                    return Err(Error::Cancelled);
                }
            }
            Ok(())
        };
        tokio::time::timeout(SEGMENT_TIMEOUT, wait)
            .await
            .map_err(|_| Error::Timeout("waiting for a segment"))??;
        Ok(tokio::fs::read(self.inner.segment_path(track, index))
            .await?
            .into())
    }
}

impl State {
    /// The error of the run a request started itself, if that run failed.
    /// Failures of other runs (since replaced) are not this request's.
    fn failed(&self, started: Option<u64>) -> Option<Error> {
        let err = self.error.as_ref()?;
        (started == Some(self.generation)).then(|| Error::Pipeline(err.clone()))
    }
}

impl Inner {
    fn segment_path(&self, track: Track, index: usize) -> PathBuf {
        self.opts.dir.join(format!("{}_{index}.m4s", track.name()))
    }

    /// Replaces the current run with one starting at segment `start`.
    /// Returns the new run's generation.
    fn start_run(self: &Arc<Self>, state: &mut State, start: usize) -> Result<u64> {
        if let Some(old) = state.run.take() {
            old.stop();
        }
        state.generation += 1;
        let generation = state.generation;
        tracing::debug!(dir = %self.opts.dir.display(), start, "Starting packaging run");
        if let Some(on_seek) = &self.opts.on_seek {
            on_seek(Duration::from_nanos(self.boundaries[start]));
        }

        let weak = Arc::downgrade(self);
        let spec = run::RunSpec {
            input: self.opts.input.clone(),
            info: self.opts.info.clone(),
            video: self.opts.video,
            audio: self.opts.audio,
            audio_index: self.opts.audio_index,
            encoder: self.opts.encoder.clone(),
            boundaries: self.boundaries.clone(),
            start,
        };
        let run = run::start(spec, move |output| {
            if let Some(inner) = weak.upgrade() {
                inner.on_output(generation, output);
            }
        })?;
        state.run = Some(run);
        state.error = None;
        Ok(generation)
    }

    /// Called from streaming threads with each piece a run produces.
    fn on_output(&self, generation: u64, output: run::Output) {
        // Files are written before taking the lock; a stale run's writes are
        // identical bytes for the same segment, so they're harmless.
        let mut written: Option<(Track, usize)> = None;
        if let run::Output::Fragment(track, pts, data) = &output {
            let index = index_at(&self.boundaries, *pts);
            let path = self.segment_path(*track, index);
            let tmp = path.with_extension(format!("tmp{generation}"));
            if std::fs::write(&tmp, data)
                .and_then(|_| std::fs::rename(&tmp, &path))
                .is_ok()
            {
                written = Some((*track, index));
            } else {
                let _ = std::fs::remove_file(&tmp);
            }
        }

        let mut state = self.state.lock().unwrap();
        let current = state.generation == generation;
        match output {
            run::Output::Init(track, data) => {
                state.init[track as usize].get_or_insert(data);
            }
            run::Output::Fragment(..) => {
                if let Some((track, index)) = written {
                    let t = track as usize;
                    state.done[t][index] = true;
                    // A fragment that spans several boundaries (sparse
                    // keyframes with no index) leaves the skipped ones
                    // unproduced; fill them with the same bytes so requests
                    // for them don't restart the run forever.
                    let run_start = state.run.as_ref().filter(|_| current).map(|r| r.start);
                    let mut back = index;
                    while let Some(start) = run_start
                        && back > start
                        && index - back < MAX_FILL
                        && !state.done[t][back - 1]
                    {
                        back -= 1;
                        let _ = std::fs::copy(
                            self.segment_path(track, index),
                            self.segment_path(track, back),
                        );
                        state.done[t][back] = true;
                    }
                }
            }
            run::Output::Eos(track) => {
                let n = self.boundaries.len();
                if current && let Some(run) = &mut state.run {
                    let next = run.next(track);
                    if next >= n {
                        run.finished[track as usize] = true;
                    } else {
                        // The input ran out early: a truncated or damaged
                        // file. Restarting would only end the same way.
                        let err = format!("stream ended at segment {next} of {n}");
                        tracing::warn!(dir = %self.opts.dir.display(), "Packaging failed: {err}");
                        state.error = Some(err);
                        if let Some(run) = state.run.take() {
                            run.stop();
                        }
                    }
                }
            }
            run::Output::Error(err) => {
                if current {
                    tracing::warn!(dir = %self.opts.dir.display(), "Packaging failed: {err}");
                    state.error = Some(err);
                    if let Some(run) = state.run.take() {
                        run.stop();
                    }
                }
            }
        }
        drop(state);
        self.changed.send_modify(|v| *v += 1);
    }
}

/// Most segments a single fragment may stand in for. Past this something is
/// wrong, and repeating bytes would only hide it.
const MAX_FILL: usize = 4;

/// The segment a stream time (ns) falls in.
pub(crate) fn index_at(boundaries: &[u64], t: u64) -> usize {
    // A fragment starts up to a frame after its boundary; allow for
    // timestamp rounding the other way too.
    boundaries
        .partition_point(|&b| b <= t + 2_000_000)
        .saturating_sub(1)
}

impl Drop for Inner {
    fn drop(&mut self) {
        if let Some(run) = self.state.get_mut().unwrap().run.take() {
            run.stop();
        }
        let dir = self.opts.dir.clone();
        std::thread::spawn(move || {
            let _ = std::fs::remove_dir_all(dir);
        });
    }
}
