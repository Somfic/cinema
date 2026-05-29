use cinema_schema::cinema_type;
use serde::Deserialize;
use tracing::warn;

const OPENSUBTITLES_ADDON: &str = "https://opensubtitles-v3.strem.io";

#[derive(Deserialize)]
struct SubtitleResponse {
    subtitles: Vec<RawSubtitle>,
}

#[derive(Deserialize)]
struct RawSubtitle {
    id: String,
    url: String,
    lang: String,
    #[serde(default)]
    g: Option<String>,
}

#[cinema_type]
pub struct SubtitleTrack {
    pub id: String,
    pub language: String,
    pub url: String,
    /// Higher score = more likely to be in sync
    pub score: i64,
}

#[cinema_type]
pub struct SubtitleCue {
    /// Start time in seconds
    pub start: f64,
    /// End time in seconds
    pub end: f64,
    pub text: String,
}

/// Map ISO 639-1 (2-letter) to ISO 639-2/B (3-letter) codes used by OpenSubtitles
pub fn to_iso639_2(code: &str) -> &str {
    match code {
        "en" => "eng",
        "nl" => "dut",
        "fr" => "fre",
        "de" => "ger",
        "es" => "spa",
        "it" => "ita",
        "pt" => "por",
        "ru" => "rus",
        "ja" => "jpn",
        "ko" => "kor",
        "zh" => "chi",
        "ar" => "ara",
        "pl" => "pol",
        "tr" => "tur",
        "sv" => "swe",
        "no" => "nor",
        "da" => "dan",
        "fi" => "fin",
        "cs" => "cze",
        "ro" => "rum",
        "hu" => "hun",
        "el" => "gre",
        "he" => "heb",
        "th" => "tha",
        "vi" => "vie",
        "id" => "ind",
        other => other,
    }
}

/// Fetch available subtitle tracks for a movie or episode
pub async fn fetch_tracks(
    client: &reqwest::Client,
    path: &str,
    languages: &[String],
) -> Vec<SubtitleTrack> {
    let url = format!("{}/subtitles/{}.json", OPENSUBTITLES_ADDON, path);

    let res = match client.get(&url).send().await {
        Ok(r) if r.status().is_success() => r,
        Ok(r) => {
            warn!("subtitle addon returned {}", r.status());
            return vec![];
        }
        Err(e) => {
            warn!("failed to fetch subtitles: {}", e);
            return vec![];
        }
    };

    let body: SubtitleResponse = match res.json().await {
        Ok(b) => b,
        Err(e) => {
            warn!("failed to parse subtitle response: {}", e);
            return vec![];
        }
    };

    let lang_codes: Vec<&str> = languages.iter().map(|l| to_iso639_2(l)).collect();

    let mut tracks: Vec<SubtitleTrack> = body
        .subtitles
        .into_iter()
        .filter(|s| lang_codes.contains(&s.lang.as_str()))
        .map(|s| {
            let score =
                s.g.as_deref()
                    .and_then(|g| g.parse::<i64>().ok())
                    .unwrap_or(0);
            SubtitleTrack {
                id: s.id,
                language: s.lang,
                url: s.url,
                score,
            }
        })
        .collect();

    // Sort by score descending — higher score subs tend to be better synced
    tracks.sort_by(|a, b| b.score.cmp(&a.score));
    tracks
}

/// Fetch and parse an SRT file into cues
pub async fn fetch_cues(client: &reqwest::Client, srt_url: &str) -> Vec<SubtitleCue> {
    let res = match client.get(srt_url).send().await {
        Ok(r) if r.status().is_success() => r,
        Ok(r) => {
            warn!("subtitle download returned {}", r.status());
            return vec![];
        }
        Err(e) => {
            warn!("failed to download subtitle: {}", e);
            return vec![];
        }
    };

    let text = match res.text().await {
        Ok(t) => t,
        Err(e) => {
            warn!("failed to read subtitle body: {}", e);
            return vec![];
        }
    };

    parse_srt(&text)
}

/// Parse SRT format into subtitle cues
pub fn parse_srt(input: &str) -> Vec<SubtitleCue> {
    let mut cues = Vec::new();
    let input = input.trim_start_matches('\u{feff}'); // strip BOM
    // Normalize line endings: \r\n → \n
    let input = input.replace("\r\n", "\n");

    for block in input.split("\n\n") {
        let lines: Vec<&str> = block.trim().lines().collect();
        if lines.len() < 3 {
            continue;
        }

        // Line 0: sequence number (skip)
        // Line 1: timestamps "00:01:51,822 --> 00:01:53,790"
        // Line 2+: text
        let Some((start, end)) = parse_srt_timestamps(lines[1]) else {
            continue;
        };

        let text = clean_sdh(&strip_html_tags(&lines[2..].join("\n")));

        if !text.is_empty() {
            cues.push(SubtitleCue { start, end, text });
        }
    }

    cues
}

/// Strip SDH annotations: speaker labels (WOMAN:), bracketed/parenthesized descriptions ([humming], (sighs))
fn clean_sdh(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    for line in s.lines() {
        let mut cleaned = String::with_capacity(line.len());
        let mut chars = line.chars().peekable();
        // Strip bracketed [...] and parenthesized (...) descriptions
        let mut depth_square = 0i32;
        let mut depth_paren = 0i32;
        while let Some(c) = chars.next() {
            match c {
                '[' => depth_square += 1,
                ']' => {
                    depth_square = (depth_square - 1).max(0);
                }
                '(' => depth_paren += 1,
                ')' => {
                    depth_paren = (depth_paren - 1).max(0);
                }
                _ if depth_square > 0 || depth_paren > 0 => {}
                _ => cleaned.push(c),
            }
        }
        // Strip speaker labels: "WORD:" or "WORD WORD:" at the start
        let trimmed = cleaned.trim();
        let after_label = if let Some(colon_pos) = trimmed.find(':') {
            let before = &trimmed[..colon_pos];
            let is_label = !before.is_empty()
                && before
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c == ' ' || c == '-');
            if is_label {
                trimmed[colon_pos + 1..].trim()
            } else {
                trimmed
            }
        } else {
            trimmed
        };
        // Skip lines that are just music notes (♪, ♫, #)
        let is_music = after_label
            .chars()
            .all(|c| c == '♪' || c == '♫' || c == '#' || c == ' ' || c == '~');
        if !after_label.is_empty() && !is_music {
            if !result.is_empty() {
                result.push('\n');
            }
            result.push_str(after_label);
        }
    }
    result
}

/// Strip HTML/font tags from subtitle text
fn strip_html_tags(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        if c == '<' {
            in_tag = true;
        } else if c == '>' {
            in_tag = false;
        } else if !in_tag {
            result.push(c);
        }
    }
    result.trim().to_string()
}

/// Parse "00:01:51,822 --> 00:01:53,790" into (start_secs, end_secs)
fn parse_srt_timestamps(line: &str) -> Option<(f64, f64)> {
    let parts: Vec<&str> = line.split("-->").collect();
    if parts.len() != 2 {
        return None;
    }
    let start = parse_srt_time(parts[0].trim())?;
    let end = parse_srt_time(parts[1].trim())?;
    Some((start, end))
}

/// Parse "00:01:51,822" into seconds
fn parse_srt_time(s: &str) -> Option<f64> {
    // Handle both "00:01:51,822" and "00:01:51.822"
    let s = s.replace(',', ".");
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 3 {
        return None;
    }
    let hours: f64 = parts[0].parse().ok()?;
    let minutes: f64 = parts[1].parse().ok()?;
    let seconds: f64 = parts[2].parse().ok()?;
    Some(hours * 3600.0 + minutes * 60.0 + seconds)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_srt() {
        let srt = "1\n00:00:01,000 --> 00:00:03,000\nHello world\n\n2\n00:00:05,500 --> 00:00:07,800\nSecond line\nWith two lines\n";
        let cues = parse_srt(srt);
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0].start, 1.0);
        assert_eq!(cues[0].end, 3.0);
        assert_eq!(cues[0].text, "Hello world");
        assert_eq!(cues[1].start, 5.5);
        assert_eq!(cues[1].end, 7.8);
        assert_eq!(cues[1].text, "Second line\nWith two lines");
    }

    #[test]
    fn test_parse_srt_with_bom() {
        let srt = "\u{feff}1\n00:00:01,000 --> 00:00:02,000\nTest\n";
        let cues = parse_srt(srt);
        assert_eq!(cues.len(), 1);
    }

    #[test]
    fn test_clean_sdh_speaker_labels() {
        assert_eq!(
            clean_sdh("WOMAN: Could you pass that?"),
            "Could you pass that?"
        );
        assert_eq!(clean_sdh("JOHN: Hello there"), "Hello there");
        assert_eq!(clean_sdh("MR SMITH: Welcome"), "Welcome");
    }

    #[test]
    fn test_clean_sdh_brackets() {
        assert_eq!(clean_sdh("[humming]"), "");
        assert_eq!(clean_sdh("(sighs) I'm tired"), "I'm tired");
        assert_eq!(clean_sdh("[door closes] Come in"), "Come in");
        assert_eq!(clean_sdh("Hello [laughs] there"), "Hello  there");
    }

    #[test]
    fn test_clean_sdh_music() {
        assert_eq!(clean_sdh("♪ ♪"), "");
        assert_eq!(clean_sdh("♪♪"), "");
        assert_eq!(clean_sdh("# Some lyrics #"), "# Some lyrics #");
    }

    #[test]
    fn test_clean_sdh_preserves_normal() {
        assert_eq!(clean_sdh("Just a normal line"), "Just a normal line");
        assert_eq!(clean_sdh("What time is it?"), "What time is it?");
    }
}
