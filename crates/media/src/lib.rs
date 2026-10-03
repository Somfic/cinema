//! Everything cinema does with media bytes, on GStreamer: probing files,
//! extracting subtitles, packaging them as on-demand HLS for live playback,
//! and transcoding them to MP4 in the background.
//!
//! The crate knows nothing about torrents or the database. Callers describe
//! where the bytes are with an [`Input`] - a file on disk, or anything that
//! can open an async reader - and get back plain data.

mod error;
mod input;
mod keyframes;
mod letterbox;
mod pipeline;
mod plan;
mod probe;
mod subtitles;

pub mod file;
pub mod hls;

pub use error::{Error, Result};
pub use input::{BoxReader, Input, Opener};
pub use keyframes::keyframes;
pub use letterbox::content_aspect;
pub use pipeline::{EncoderSettings, Hardware, h264_encoder_name};
pub use plan::{AudioAction, ClientCaps, Plan, PlanRequest, VideoAction, plan};
pub use probe::{AudioTrack, Chapter, MediaInfo, SubtitleTrack, VideoTrack, probe};
pub use subtitles::{Cue, extract_subtitles};

static INIT: std::sync::OnceLock<std::result::Result<(), String>> = std::sync::OnceLock::new();

/// Initialise GStreamer and register the statically linked plugins. Cheap and
/// idempotent; every entry point calls it, so callers only need it to fail
/// fast at startup.
pub fn init() -> Result<()> {
    INIT.get_or_init(|| {
        gst::init().map_err(|e| e.to_string())?;
        gstfmp4::plugin_register_static().map_err(|e| e.to_string())?;
        Ok(())
    })
    .clone()
    .map_err(Error::Init)
}
