use std::collections::HashMap;

use futures::future::join_all;
use serde::Deserialize;
use tracing::warn;

// --- Raw Stremio addon response types ---

#[derive(Deserialize)]
struct StremioResponse {
    streams: Vec<RawStream>,
}

#[derive(Deserialize)]
struct RawStream {
    name: Option<String>,
    title: Option<String>,
    #[serde(rename = "infoHash")]
    info_hash: Option<String>,
    #[serde(rename = "fileIdx")]
    file_idx: Option<i32>,
    #[serde(rename = "behaviorHints")]
    behavior_hints: Option<BehaviorHints>,
}

#[derive(Deserialize)]
struct BehaviorHints {
    #[serde(rename = "bingeGroup")]
    binge_group: Option<String>,
    filename: Option<String>,
}

// --- Enriched public types ---

#[draad::ty]
pub struct Stream {
    pub info_hash: String,
    pub file_idx: i32,
    pub name: String,
    pub title: String,
    pub source: String,
    pub resolution: Option<String>,
    pub codec: Option<String>,
    pub audio: Option<String>,
    pub source_type: Option<String>,
    pub hdr: bool,
    pub imax: bool,
    pub seeders: Option<u32>,
    pub size_bytes: Option<u64>,
    pub size_display: Option<String>,
    pub score: f64,
}

// --- Aggregation ---

pub enum AggregationMediaType {
    Media {
        imdb_id: String,
    },
    Tv {
        imdb_id: String,
        season: u32,
        episode: u32,
    },
}

impl AggregationMediaType {
    pub async fn aggregate(self, client: &reqwest::Client, sources: &[String]) -> Vec<Stream> {
        let path = match self {
            AggregationMediaType::Media { imdb_id } => format!("movie/{imdb_id}"),
            AggregationMediaType::Tv {
                imdb_id,
                season,
                episode,
            } => format!("series/{imdb_id}:{season}:{episode}"),
        };

        let futures: Vec<_> = sources
            .iter()
            .map(|source| fetch_source(client, source, &path))
            .collect();

        let results = join_all(futures).await;

        let mut by_hash: HashMap<String, Stream> = HashMap::new();

        for (source_name, streams) in results.into_iter().flatten() {
            for stream in streams {
                by_hash
                    .entry(stream.info_hash.clone())
                    .and_modify(|existing| {
                        // Keep the one with more seeders; merge source names
                        if !existing.source.contains(&source_name) {
                            existing.source = format!("{}, {}", existing.source, source_name);
                        }
                        if stream.seeders > existing.seeders {
                            existing.seeders = stream.seeders;
                            existing.score = compute_score(&*existing);
                        }
                    })
                    .or_insert(stream);
            }
        }

        let mut streams: Vec<Stream> = by_hash.into_values().collect();
        streams.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        streams
    }
}

async fn fetch_source(
    client: &reqwest::Client,
    source: &str,
    path: &str,
) -> Option<(String, Vec<Stream>)> {
    let url = format!("{}/stream/{}.json", source.trim_end_matches('/'), path);
    let source_name = extract_source_name(source);

    let res = match client.get(&url).send().await {
        Ok(r) => r,
        Err(e) => {
            warn!("failed to fetch from {}: {}", source_name, e);
            return None;
        }
    };

    if !res.status().is_success() {
        warn!("{} returned {}", source_name, res.status());
        return None;
    }

    let body: StremioResponse = match res.json().await {
        Ok(b) => b,
        Err(e) => {
            warn!("failed to parse response from {}: {}", source_name, e);
            return None;
        }
    };

    let streams: Vec<Stream> = body
        .streams
        .into_iter()
        .filter_map(|raw| {
            let info_hash = raw.info_hash?;
            let name = raw.name.unwrap_or_default();
            let title = raw.title.unwrap_or_default();
            let file_idx = raw.file_idx.unwrap_or(0);

            let binge_group = raw
                .behavior_hints
                .as_ref()
                .and_then(|h| h.binge_group.as_deref());
            let filename = raw
                .behavior_hints
                .as_ref()
                .and_then(|h| h.filename.as_deref());

            let resolution = parse_resolution(binge_group, &name);
            let codec = parse_codec(binge_group, filename);
            let audio = parse_audio(filename, &title);
            let source_type = parse_source_type(binge_group, &title);
            let hdr = parse_hdr(binge_group, &title);
            let imax = parse_imax(binge_group, &title, filename);
            let seeders = parse_seeders(&title);
            let (size_bytes, size_display) = parse_size(&title);

            let mut stream = Stream {
                info_hash,
                file_idx,
                name,
                title,
                source: source_name.clone(),
                resolution,
                codec,
                audio,
                source_type,
                hdr,
                imax,
                seeders,
                size_bytes,
                size_display,
                score: 0.0,
            };
            stream.score = compute_score(&stream);
            Some(stream)
        })
        .collect();

    Some((source_name, streams))
}

fn extract_source_name(url: &str) -> String {
    // "https://torrentio.strem.fun" → "Torrentio"
    // "https://mediafusion.elfhosted.com" → "MediaFusion"
    url.trim_start_matches("https://")
        .trim_start_matches("http://")
        .split('.')
        .next()
        .unwrap_or("unknown")
        .to_string()
}

// --- Parsing ---

fn parse_seeders(title: &str) -> Option<u32> {
    for line in title.split('\n') {
        if let Some(pos) = line.find('👤') {
            let after = &line[pos + '👤'.len_utf8()..];
            let num_str: String = after
                .trim()
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            return num_str.parse().ok();
        }
    }
    None
}

fn parse_size(title: &str) -> (Option<u64>, Option<String>) {
    for line in title.split('\n') {
        if let Some(pos) = line.find('💾') {
            let after = &line[pos + '💾'.len_utf8()..].trim_start();
            let parts: Vec<&str> = after.splitn(3, ' ').collect();
            if parts.len() >= 2
                && let Ok(num) = parts[0].parse::<f64>()
            {
                let unit = parts[1].to_uppercase();
                let display = format!("{} {}", parts[0], parts[1]);
                let bytes = match unit.as_str() {
                    "TB" => Some((num * 1_099_511_627_776.0) as u64),
                    "GB" => Some((num * 1_073_741_824.0) as u64),
                    "MB" => Some((num * 1_048_576.0) as u64),
                    "KB" => Some((num * 1_024.0) as u64),
                    _ => None,
                };
                return (bytes, Some(display));
            }
        }
    }
    (None, None)
}

fn parse_resolution(binge_group: Option<&str>, name: &str) -> Option<String> {
    let sources = [binge_group.unwrap_or(""), name];
    for text in sources {
        let lower = text.to_lowercase();
        for segment in lower.split('|').chain(lower.split_whitespace()) {
            let s = segment.trim();
            if s == "4k" || s == "2160p" {
                return Some("2160p".into());
            }
            if s == "1440p" {
                return Some("1440p".into());
            }
            if s == "1080p" {
                return Some("1080p".into());
            }
            if s == "720p" {
                return Some("720p".into());
            }
            if s == "480p" {
                return Some("480p".into());
            }
        }
    }
    None
}

fn parse_codec(binge_group: Option<&str>, filename: Option<&str>) -> Option<String> {
    let text = format!("{}|{}", binge_group.unwrap_or(""), filename.unwrap_or("")).to_lowercase();

    if text.contains("x265")
        || text.contains("hevc")
        || text.contains("h265")
        || text.contains("h.265")
    {
        Some("x265".into())
    } else if text.contains("x264")
        || text.contains("avc")
        || text.contains("h264")
        || text.contains("h.264")
    {
        Some("x264".into())
    } else if text.contains("av1") {
        Some("AV1".into())
    } else {
        None
    }
}

fn parse_audio(filename: Option<&str>, title: &str) -> Option<String> {
    let text = format!("{} {}", filename.unwrap_or(""), title).to_lowercase();

    // Check in priority order (most specific first)
    let patterns = [
        ("truehd", "TrueHD"),
        ("atmos", "Atmos"),
        ("dts-hd", "DTS-HD"),
        ("dts-x", "DTS-X"),
        ("dtsx", "DTS-X"),
        ("dts", "DTS"),
        ("ddp5.1", "DDP 5.1"),
        ("dd+5.1", "DDP 5.1"),
        ("ddp", "DDP"),
        ("dd5.1", "DD 5.1"),
        ("dd+", "DDP"),
        ("eac3", "EAC3"),
        ("ac3", "AC3"),
        ("aac5.1", "AAC 5.1"),
        ("aac2.0", "AAC"),
        ("aac", "AAC"),
        ("flac", "FLAC"),
        ("opus", "Opus"),
    ];

    for (pattern, label) in patterns {
        if text.contains(pattern) {
            return Some(label.into());
        }
    }
    None
}

fn parse_source_type(binge_group: Option<&str>, title: &str) -> Option<String> {
    let text = format!("{}|{}", binge_group.unwrap_or(""), title).to_lowercase();

    if text.contains("remux") {
        Some("BluRay REMUX".into())
    } else if text.contains("bluray") || text.contains("blu-ray") {
        Some("BluRay".into())
    } else if text.contains("web-dl") || text.contains("webdl") {
        Some("WEB-DL".into())
    } else if text.contains("webrip") {
        Some("WEBRip".into())
    } else if text.contains("hdrip") {
        Some("HDRip".into())
    } else if text.contains("bdrip") {
        Some("BDRip".into())
    } else if text.contains("brrip") {
        Some("BRRip".into())
    } else if text.contains("dvdrip") {
        Some("DVDRip".into())
    } else if text.contains("hdtv") {
        Some("HDTV".into())
    } else if text.contains("cam") || text.contains("hdcam") {
        Some("CAM".into())
    } else {
        None
    }
}

fn parse_hdr(binge_group: Option<&str>, title: &str) -> bool {
    let text = format!("{}|{}", binge_group.unwrap_or(""), title).to_lowercase();
    text.contains("hdr")
        || text.contains("dolby vision")
        || text.contains("|dv|")
        || text.contains("|dv")
}

fn parse_imax(binge_group: Option<&str>, title: &str, filename: Option<&str>) -> bool {
    let text = format!(
        "{}|{}|{}",
        binge_group.unwrap_or(""),
        title,
        filename.unwrap_or("")
    )
    .to_lowercase();
    text.contains("imax")
}

// --- Scoring ---

fn compute_score(stream: &Stream) -> f64 {
    let resolution_weight = match stream.resolution.as_deref() {
        Some("2160p") => 100.0,
        Some("1440p") => 90.0,
        Some("1080p") => 80.0,
        Some("720p") => 40.0,
        Some("480p") => 10.0,
        _ => 20.0,
    };

    let seeder_factor = match stream.seeders {
        Some(s) if s > 0 => ((s as f64) + 1.0).log2() / 10.0,
        Some(0) | None => 0.3,
        _ => 0.3,
    };

    let source_weight = match stream.source_type.as_deref() {
        Some("BluRay REMUX") => 1.0,
        Some("BluRay") => 0.95,
        Some("WEB-DL") => 0.9,
        Some("WEBRip") => 0.8,
        Some("BDRip") | Some("BRRip") => 0.7,
        Some("HDRip") => 0.6,
        Some("DVDRip") | Some("HDTV") => 0.4,
        Some("CAM") => 0.1,
        _ => 0.5,
    };

    let audio_factor = match stream.audio.as_deref() {
        Some("AAC") | Some("AAC 5.1") => 1.0,
        Some("AC3") | Some("DD 5.1") => 0.95,
        Some("EAC3") | Some("DDP") | Some("DDP 5.1") => 0.9,
        Some("DTS") | Some("DTS-HD") | Some("DTS-X") => 0.7,
        Some("TrueHD") | Some("Atmos") => 0.7,
        Some("FLAC") => 0.8,
        _ => 0.85,
    };

    let size_factor = match stream.size_bytes {
        Some(b) if b > 80_000_000_000 => 0.5,
        Some(b) if b > 40_000_000_000 => 0.7,
        _ => 1.0,
    };

    resolution_weight * seeder_factor * source_weight * audio_factor * size_factor
}
