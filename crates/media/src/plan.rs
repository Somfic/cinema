//! Decides how little work a client needs: play the file as is, repackage
//! it, or re-encode just the streams the client can't decode.

use crate::MediaInfo;

/// What a playback client can decode, in [`crate::probe`]'s codec names.
#[derive(Clone, Debug, Default)]
pub struct ClientCaps {
    /// `h264`, `hevc`, `av1`, `vp9`.
    pub video_codecs: Vec<String>,
    /// `aac`, `ac3`, `eac3`, `opus`, `flac`, `mp3`.
    pub audio_codecs: Vec<String>,
    /// Containers it plays from a plain URL: `mp4`, `webm`, ...
    pub containers: Vec<String>,
    /// Tallest video it decodes; `None` for no limit.
    pub max_height: Option<u32>,
}

#[derive(Clone, Debug, Default)]
pub struct PlanRequest {
    pub audio_index: usize,
    /// Re-encode video even when the client could decode it.
    pub force_video_transcode: bool,
    /// Re-encode audio even when the client could decode it.
    pub force_audio_transcode: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoAction {
    Copy,
    Transcode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioAction {
    Copy,
    Transcode,
    /// The file has no audio.
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Plan {
    /// The client plays the original file over HTTP.
    Direct,
    /// Packaged as on-demand HLS, each stream copied or re-encoded.
    Hls {
        video: VideoAction,
        audio: AudioAction,
    },
}

/// Codecs the HLS packager can carry without re-encoding.
const PACKAGEABLE_VIDEO: &[&str] = &["h264", "hevc", "av1", "vp9"];
const PACKAGEABLE_AUDIO: &[&str] = &["aac", "ac3", "eac3", "opus", "flac"];

pub fn plan(info: &MediaInfo, caps: &ClientCaps, req: &PlanRequest) -> Plan {
    let has = |list: &[String], codec: &str| list.iter().any(|c| c.eq_ignore_ascii_case(codec));

    let video_ok = info.video.first().is_some_and(|v| {
        has(&caps.video_codecs, &v.codec)
            // 10-bit H.264 (Hi10P) decodes almost nowhere, and nothing
            // consumer-grade decodes chroma beyond 4:2:0.
            && !(v.codec == "h264" && v.bit_depth > 8)
            && v.chroma_format.as_deref().is_none_or(|c| c == "4:2:0")
            && caps.max_height.is_none_or(|max| v.height <= max)
    }) && !req.force_video_transcode;

    let audio = info.audio.get(req.audio_index).or(info.audio.first());
    let audio_ok =
        audio.is_none_or(|a| has(&caps.audio_codecs, &a.codec)) && !req.force_audio_transcode;

    let container_ok = info
        .container
        .as_deref()
        .is_some_and(|c| has(&caps.containers, c));
    // A plain URL always plays the first audio track.
    let default_audio = req.audio_index == 0 || info.audio.len() <= 1;

    if video_ok && audio_ok && container_ok && default_audio {
        return Plan::Direct;
    }

    let video = match info.video.first() {
        Some(v) if video_ok && PACKAGEABLE_VIDEO.contains(&v.codec.as_str()) => VideoAction::Copy,
        _ => VideoAction::Transcode,
    };
    let audio = match audio {
        None => AudioAction::None,
        Some(a) if audio_ok && PACKAGEABLE_AUDIO.contains(&a.codec.as_str()) => AudioAction::Copy,
        Some(_) => AudioAction::Transcode,
    };
    Plan::Hls { video, audio }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AudioTrack, VideoTrack};

    fn info(container: &str, video: &str, audio: &[&str]) -> MediaInfo {
        MediaInfo {
            container: Some(container.into()),
            video: vec![VideoTrack {
                codec: video.into(),
                width: 1920,
                height: 1080,
                bit_depth: 8,
                chroma_format: None,
                mime_codec: None,
            }],
            audio: audio
                .iter()
                .enumerate()
                .map(|(i, c)| AudioTrack {
                    index: i + 1,
                    stream_index: i,
                    codec: (*c).into(),
                    channels: 6,
                    language: None,
                    title: None,
                    mime_codec: None,
                })
                .collect(),
            ..Default::default()
        }
    }

    fn browser() -> ClientCaps {
        ClientCaps {
            video_codecs: vec!["h264".into(), "hevc".into()],
            audio_codecs: vec!["aac".into(), "opus".into()],
            containers: vec!["mp4".into(), "webm".into()],
            max_height: None,
        }
    }

    #[test]
    fn plays_compatible_mp4_directly() {
        let p = plan(
            &info("mp4", "h264", &["aac"]),
            &browser(),
            &PlanRequest::default(),
        );
        assert_eq!(p, Plan::Direct);
    }

    #[test]
    fn remuxes_matroska_without_encoding() {
        let p = plan(
            &info("matroska", "h264", &["aac"]),
            &browser(),
            &PlanRequest::default(),
        );
        assert_eq!(
            p,
            Plan::Hls {
                video: VideoAction::Copy,
                audio: AudioAction::Copy
            }
        );
    }

    #[test]
    fn re_encodes_only_the_audio_the_client_lacks() {
        let p = plan(
            &info("matroska", "hevc", &["eac3"]),
            &browser(),
            &PlanRequest::default(),
        );
        assert_eq!(
            p,
            Plan::Hls {
                video: VideoAction::Copy,
                audio: AudioAction::Transcode
            }
        );
    }

    #[test]
    fn second_audio_track_needs_packaging() {
        let req = PlanRequest {
            audio_index: 1,
            ..Default::default()
        };
        let p = plan(&info("mp4", "h264", &["aac", "aac"]), &browser(), &req);
        assert!(matches!(p, Plan::Hls { .. }));
    }

    #[test]
    fn high_444_is_re_encoded() {
        let mut i = info("mp4", "h264", &["aac"]);
        i.video[0].chroma_format = Some("4:4:4".into());
        let p = plan(&i, &browser(), &PlanRequest::default());
        assert!(matches!(
            p,
            Plan::Hls {
                video: VideoAction::Transcode,
                ..
            }
        ));
    }

    #[test]
    fn hi10p_is_re_encoded() {
        let mut i = info("matroska", "h264", &["aac"]);
        i.video[0].bit_depth = 10;
        let p = plan(&i, &browser(), &PlanRequest::default());
        assert_eq!(
            p,
            Plan::Hls {
                video: VideoAction::Transcode,
                audio: AudioAction::Copy
            }
        );
    }
}
