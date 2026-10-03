use core::time::Duration;
use std::env;
use std::path::PathBuf;

use serde::Deserialize;

use crate::app::{CinemaError, Result};

#[derive(Deserialize)]
pub struct Config {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,
    pub database_url: Option<String>,

    #[serde(default)]
    pub tmdb_api_key: String,
    #[serde(default = "default_stream_sources")]
    pub stream_sources: Vec<String>,
    #[serde(default = "default_subtitle_languages")]
    pub subtitle_languages: Vec<String>,
    #[serde(default = "default_max_concurrent_downloads")]
    pub max_concurrent_downloads: usize,
    #[serde(default = "default_max_concurrent_pretranscodings")]
    pub max_concurrent_pretranscodings: usize,
    #[serde(default = "default_torrent_listen_port")]
    pub torrent_port: u16,
    #[serde(default = "default_dht_enabled")]
    pub use_dht: bool,

    #[serde(default = "default_torrent_validation_timeout")]
    pub torrent_validation_timeout: Duration,

    /// Video encoder family: `auto`, `none` (software x264), `nvidia`,
    /// `vaapi` or `videotoolbox`.
    #[serde(default = "default_transcode_hardware")]
    pub transcode_hardware: String,
    /// x264 speed preset, used when encoding in software.
    #[serde(default = "default_transcode_preset")]
    pub transcode_preset: String,
    /// Constant-quality target on the CRF scale (lower is better).
    #[serde(default = "default_transcode_crf")]
    pub transcode_crf: u8,
    /// Re-encoded video is scaled down to at most this height; 0 keeps the
    /// source resolution.
    #[serde(default)]
    pub transcode_max_height: u32,
}

impl Config {
    pub fn encoder(&self) -> media::EncoderSettings {
        media::EncoderSettings {
            hardware: self.transcode_hardware.parse().unwrap_or_default(),
            preset: self.transcode_preset.clone(),
            crf: self.transcode_crf,
            max_height: match self.transcode_max_height {
                0 => u32::MAX,
                h => h,
            },
        }
    }
}

impl Config {
    pub fn from_file(path: impl AsRef<std::path::Path>) -> Result<Self> {
        let path = path.as_ref();
        match std::fs::read_to_string(path) {
            Ok(content) => Ok(toml::from_str(&content)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(toml::from_str("")?),
            Err(e) => Err(CinemaError::ConfigReadError {
                path: path.display().to_string(),
                source: e,
            }),
        }
    }

    pub fn apply_env_overrides(&mut self) {
        if let Ok(v) = env::var("CINEMA_TMDB_API_KEY") {
            self.tmdb_api_key = v;
        }
        if let Ok(v) = env::var("CINEMA_STREAM_SOURCES") {
            self.stream_sources = v.split(',').map(|s| s.trim().to_string()).collect();
        }
        if let Ok(v) = env::var("CINEMA_SUBTITLE_LANGUAGES") {
            self.subtitle_languages = v.split(',').map(|s| s.trim().to_string()).collect();
        }
        if let Ok(v) = env::var("CINEMA_MAX_CONCURRENT_DOWNLOADS")
            && let Ok(n) = v.parse()
        {
            self.max_concurrent_downloads = n;
        }
        if let Ok(v) = env::var("CINEMA_MAX_CONCURRENT_PRETRANSCODINGS")
            && let Ok(n) = v.parse()
        {
            self.max_concurrent_pretranscodings = n;
        }
        if let Ok(v) = env::var("CINEMA_TORRENT_PORT")
            && let Ok(n) = v.parse()
        {
            self.torrent_port = n;
        }
        if let Ok(v) = env::var("CINEMA_USE_DHT")
            && let Ok(b) = v.parse()
        {
            self.use_dht = b;
        }
        if let Ok(v) = env::var("CINEMA_TORRENT_VALIDATION_TIMEOUT_MS")
            && let Ok(d_ms) = v.parse()
        {
            self.torrent_validation_timeout = Duration::from_millis(d_ms);
        }
        if let Ok(v) = env::var("CINEMA_TRANSCODE_HARDWARE") {
            self.transcode_hardware = v;
        }
        if let Ok(v) = env::var("CINEMA_TRANSCODE_PRESET") {
            self.transcode_preset = v;
        }
        if let Ok(v) = env::var("CINEMA_TRANSCODE_CRF")
            && let Ok(n) = v.parse()
        {
            self.transcode_crf = n;
        }
        if let Ok(v) = env::var("CINEMA_TRANSCODE_MAX_HEIGHT")
            && let Ok(n) = v.parse()
        {
            self.transcode_max_height = n;
        }
    }
}

fn default_host() -> String {
    "0.0.0.0".to_string()
}

fn default_port() -> u16 {
    3000
}

fn default_data_dir() -> PathBuf {
    PathBuf::from("./data/")
}

fn default_max_concurrent_downloads() -> usize {
    2
}

fn default_max_concurrent_pretranscodings() -> usize {
    // A single GPU is the bottleneck for full transcodes, and only-audio
    // jobs are cheap enough not to need a bigger cap.
    1
}

fn default_subtitle_languages() -> Vec<String> {
    vec!["en".to_string()]
}

fn default_stream_sources() -> Vec<String> {
    vec!["https://torrentio.strem.fun".to_string()]
}

fn default_torrent_listen_port() -> u16 {
    6881
}

fn default_dht_enabled() -> bool {
    true
}

fn default_torrent_validation_timeout() -> Duration {
    Duration::from_secs(30)
}

fn default_transcode_hardware() -> String {
    "auto".to_string()
}

fn default_transcode_preset() -> String {
    // `ultrafast` would turn off x264's deblocking filter, which shows as
    // blocks on a big screen. Slow machines (a Raspberry Pi 5) can still
    // opt into it.
    "veryfast".to_string()
}

fn default_transcode_crf() -> u8 {
    // Visually lossless; there's bandwidth to spare on a home network.
    18
}
