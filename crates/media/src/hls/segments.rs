//! Segment boundaries and the playlists that describe them.

use std::fmt::Write;

/// Segment length to aim for. Copied video can only cut on the file's own
/// keyframes, so its segments run longer where keyframes are sparse.
const TARGET: u64 = 4_000_000_000;

/// A final segment shorter than this is folded into the one before it.
const MIN_TAIL: u64 = 1_000_000_000;

/// Start time (ns) of every segment; the first is always 0. With `keyframes`
/// every boundary is a keyframe, so copied video splits exactly there.
pub(crate) fn boundaries(duration: u64, keyframes: Option<&[u64]>) -> Vec<u64> {
    let mut out = vec![0];
    let mut last = 0;
    let mut push = |t: u64| {
        if t >= last + TARGET && t + MIN_TAIL < duration {
            out.push(t);
            last = t;
        }
    };
    match keyframes {
        Some(keyframes) => keyframes.iter().copied().for_each(&mut push),
        None => (1..)
            .map(|i| i * TARGET)
            .take_while(|&t| t < duration)
            .for_each(&mut push),
    }
    out
}

pub(crate) fn durations(boundaries: &[u64], duration: u64) -> impl Iterator<Item = u64> + '_ {
    boundaries.iter().enumerate().map(move |(i, &start)| {
        boundaries
            .get(i + 1)
            .copied()
            .unwrap_or(duration)
            .saturating_sub(start)
    })
}

/// A VOD media playlist listing every segment up front, so players treat the
/// whole file as seekable before any of it has been packaged.
pub(crate) fn media_playlist(track: &str, boundaries: &[u64], duration: u64) -> String {
    let target = durations(boundaries, duration).max().unwrap_or(TARGET);
    let mut out = String::new();
    let _ = writeln!(out, "#EXTM3U");
    let _ = writeln!(out, "#EXT-X-VERSION:7");
    let _ = writeln!(
        out,
        "#EXT-X-TARGETDURATION:{}",
        target.div_ceil(1_000_000_000)
    );
    let _ = writeln!(out, "#EXT-X-PLAYLIST-TYPE:VOD");
    let _ = writeln!(out, "#EXT-X-MEDIA-SEQUENCE:0");
    let _ = writeln!(out, "#EXT-X-INDEPENDENT-SEGMENTS");
    let _ = writeln!(out, "#EXT-X-MAP:URI=\"{track}_init.mp4\"");
    for (i, d) in durations(boundaries, duration).enumerate() {
        let _ = writeln!(out, "#EXTINF:{:.6},", d as f64 / 1e9);
        let _ = writeln!(out, "{track}_{i}.m4s");
    }
    let _ = writeln!(out, "#EXT-X-ENDLIST");
    out
}

pub(crate) struct MasterInfo<'a> {
    pub video_codec: Option<&'a str>,
    pub audio_codec: Option<&'a str>,
    pub resolution: Option<(u32, u32)>,
    pub bandwidth: u64,
    pub audio: Option<AudioRendition<'a>>,
}

pub(crate) struct AudioRendition<'a> {
    pub name: &'a str,
    pub language: Option<&'a str>,
    pub channels: u32,
}

pub(crate) fn master_playlist(info: &MasterInfo) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "#EXTM3U");
    let _ = writeln!(out, "#EXT-X-VERSION:7");
    let _ = writeln!(out, "#EXT-X-INDEPENDENT-SEGMENTS");
    if let Some(audio) = &info.audio {
        let mut line = format!(
            "#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"audio\",NAME=\"{}\",DEFAULT=YES,AUTOSELECT=YES,CHANNELS=\"{}\"",
            audio.name.replace('"', "'"),
            audio.channels
        );
        if let Some(lang) = audio.language {
            let _ = write!(line, ",LANGUAGE=\"{lang}\"");
        }
        let _ = writeln!(out, "{line},URI=\"audio.m3u8\"");
    }
    let mut inf = format!("#EXT-X-STREAM-INF:BANDWIDTH={}", info.bandwidth);
    let codecs: Vec<&str> = [info.video_codec, info.audio_codec]
        .into_iter()
        .flatten()
        .collect();
    // Only when every codec is known: a partial list makes players assume
    // the stream lacks the missing one.
    let complete =
        info.video_codec.is_some() && (info.audio.is_none() || info.audio_codec.is_some());
    if complete && !codecs.is_empty() {
        let _ = write!(inf, ",CODECS=\"{}\"", codecs.join(","));
    }
    if let Some((w, h)) = info.resolution {
        let _ = write!(inf, ",RESOLUTION={w}x{h}");
    }
    if info.audio.is_some() {
        inf.push_str(",AUDIO=\"audio\"");
    }
    let _ = writeln!(out, "{inf}");
    let _ = writeln!(out, "video.m3u8");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: u64 = 1_000_000_000;

    #[test]
    fn uniform_boundaries_fold_a_short_tail() {
        assert_eq!(boundaries(12 * S + S / 2, None), vec![0, 4 * S, 8 * S]);
        assert_eq!(boundaries(14 * S, None), vec![0, 4 * S, 8 * S, 12 * S]);
    }

    #[test]
    fn keyframe_boundaries_skip_close_keyframes() {
        let kfs = [0, 2 * S, 5 * S, 6 * S, 11 * S, 30 * S];
        assert_eq!(
            boundaries(40 * S, Some(&kfs)),
            vec![0, 5 * S, 11 * S, 30 * S]
        );
    }

    #[test]
    fn playlist_lists_every_segment() {
        let b = boundaries(8 * S + S / 2, None);
        let p = media_playlist("video", &b, 8 * S + S / 2);
        assert!(p.contains("#EXTINF:4.000000,\nvideo_0.m4s"));
        assert!(p.contains("#EXTINF:4.500000,\nvideo_1.m4s"));
        assert!(p.contains("#EXT-X-TARGETDURATION:5"));
        assert!(p.ends_with("#EXT-X-ENDLIST\n"));
    }
}
