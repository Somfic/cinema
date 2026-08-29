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

    #[serde(default = "default_ffmpeg_max_startup_duration")]
    pub ffmpeg_max_startup_duration: Duration,
    #[serde(default = "default_ffmpeg_startup_poll_interval")]
    pub ffmpeg_startup_poll_interval: Duration,
    #[serde(default = "default_ffmpeg_hwaccel")]
    pub ffmpeg_hwaccel: String,
    #[serde(default = "default_ffmpeg_video_preset")]
    pub ffmpeg_video_preset: String,
    #[serde(default = "default_ffmpeg_video_crf")]
    pub ffmpeg_video_crf: u8,
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
        if let Ok(v) = env::var("CINEMA_FFMPEG_MAX_STARTUP_DURATION_MS")
            && let Ok(d_ms) = v.parse()
        {
            self.ffmpeg_max_startup_duration = Duration::from_millis(d_ms);
        }
        if let Ok(v) = env::var("CINEMA_FFMPEG_STARTUP_POLL_INTERVAL_MS")
            && let Ok(d_ms) = v.parse()
        {
            self.ffmpeg_startup_poll_interval = Duration::from_millis(d_ms);
        }
        if let Ok(v) = env::var("CINEMA_FFMPEG_HWACCEL") {
            self.ffmpeg_hwaccel = v;
        }
        if let Ok(v) = env::var("CINEMA_FFMPEG_VIDEO_PRESET") {
            self.ffmpeg_video_preset = v;
        }
        if let Ok(v) = env::var("CINEMA_FFMPEG_VIDEO_CRF")
            && let Ok(n) = v.parse()
        {
            self.ffmpeg_video_crf = n;
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
    // ffmpeg + a single GPU is the bottleneck for full transcodes, and
    // only-audio jobs are cheap enough not to need a bigger cap.
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

fn default_ffmpeg_max_startup_duration() -> Duration {
    // Generous because the bottleneck at startup is the torrent delivering the
    // head of the file (a cold 4K stream can take >10s to buffer the first
    // segment), not the now hardware-accelerated encode.
    Duration::from_secs(45)
}

fn default_ffmpeg_startup_poll_interval() -> Duration {
    Duration::from_millis(100)
}

fn default_ffmpeg_hwaccel() -> String {
    "auto".to_string()
}

fn default_ffmpeg_video_preset() -> String {
    "ultrafast".to_string()
}

fn default_ffmpeg_video_crf() -> u8 {
    23
}
