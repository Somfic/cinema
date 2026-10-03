//! Video keyframe times, read from the container's own index.
//!
//! When video is copied rather than re-encoded, a segment can only start on
//! a keyframe the file already has. Knowing them up front lets the playlist
//! list exact segment durations before anything has been packaged, so
//! players can seek anywhere from the start. Only the index is read (the
//! Matroska `Cues`, the MP4 `moov`), never the media itself.

use std::io::SeekFrom;

use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::input::BoxReader;
use crate::{Input, Result};

/// Keyframe presentation times in nanoseconds, ascending. `None` when the
/// container has no usable index (MPEG-TS, fragmented MP4, Matroska without
/// cues); callers then fall back to approximate boundaries.
pub async fn keyframes(input: &Input) -> Result<Option<Vec<u64>>> {
    let (mut reader, len) = input.open_reader().await?;
    let mut head = [0u8; 12];
    if len < 12 {
        return Ok(None);
    }
    reader.as_mut().read_exact(&mut head).await?;
    let mut file = File { reader, len };
    let found = if head[..4] == [0x1A, 0x45, 0xDF, 0xA3] {
        matroska::keyframes(&mut file).await?
    } else if &head[4..8] == b"ftyp" || &head[4..8] == b"moov" {
        mp4::keyframes(&mut file).await?
    } else {
        None
    };
    Ok(found.filter(|k| !k.is_empty()).map(|mut k| {
        k.sort_unstable();
        k.dedup();
        k
    }))
}

struct File {
    reader: BoxReader,
    len: u64,
}

impl File {
    async fn read_at(&mut self, offset: u64, len: usize) -> std::io::Result<Vec<u8>> {
        self.reader.as_mut().seek(SeekFrom::Start(offset)).await?;
        let len = len.min(self.len.saturating_sub(offset) as usize);
        let mut buf = vec![0u8; len];
        self.reader.as_mut().read_exact(&mut buf).await?;
        Ok(buf)
    }
}

/// Indices larger than this are not an index; refuse instead of reading them.
const MAX_INDEX_BYTES: u64 = 64 << 20;

mod matroska {
    use super::{File, MAX_INDEX_BYTES};

    const SEGMENT: u32 = 0x1853_8067;
    const SEEK_HEAD: u32 = 0x114D_9B74;
    const SEEK: u32 = 0x4DBB;
    const SEEK_ID: u32 = 0x53AB;
    const SEEK_POSITION: u32 = 0x53AC;
    const INFO: u32 = 0x1549_A966;
    const TIMESTAMP_SCALE: u32 = 0x2A_D7B1;
    const TRACKS: u32 = 0x1654_AE6B;
    const TRACK_ENTRY: u32 = 0xAE;
    const TRACK_NUMBER: u32 = 0xD7;
    const TRACK_TYPE: u32 = 0x83;
    const CUES: u32 = 0x1C53_BB6B;
    const CUE_POINT: u32 = 0xBB;
    const CUE_TIME: u32 = 0xB3;
    const CUE_TRACK_POSITIONS: u32 = 0xB7;
    const CUE_TRACK: u32 = 0xF7;
    const CLUSTER: u32 = 0x1F43_B675;

    /// Element id (marker bits kept) and its length in bytes.
    fn read_id(b: &[u8]) -> Option<(u32, usize)> {
        let first = *b.first()?;
        let len = first.leading_zeros() as usize + 1;
        if len > 4 || b.len() < len {
            return None;
        }
        Some((b[..len].iter().fold(0u32, |a, &x| (a << 8) | x as u32), len))
    }

    /// Element size (marker removed; `None` for "unknown") and its length.
    fn read_size(b: &[u8]) -> Option<(Option<u64>, usize)> {
        let first = *b.first()?;
        let len = first.leading_zeros() as usize + 1;
        if len > 8 || b.len() < len {
            return None;
        }
        let mut v = (first as u64) & (0xFF >> len);
        let mut all_ones = v == (0xFF >> len) as u64;
        for &x in &b[1..len] {
            v = (v << 8) | x as u64;
            all_ones &= x == 0xFF;
        }
        Some((if all_ones { None } else { Some(v) }, len))
    }

    fn read_uint(b: &[u8]) -> u64 {
        b.iter().take(8).fold(0, |a, &x| (a << 8) | x as u64)
    }

    /// Iterates the child elements of an in-memory element body.
    fn children(body: &[u8]) -> impl Iterator<Item = (u32, &[u8])> {
        let mut pos = 0;
        std::iter::from_fn(move || {
            let (id, il) = read_id(&body[pos..])?;
            let (size, sl) = read_size(&body[pos + il..])?;
            let start = pos + il + sl;
            let end = start.checked_add(size? as usize)?.min(body.len());
            pos = end;
            Some((id, &body[start..end]))
        })
    }

    pub(super) async fn keyframes(file: &mut File) -> std::io::Result<Option<Vec<u64>>> {
        // EBML header, then the Segment.
        let head = file.read_at(0, 64).await?;
        let Some((_, il)) = read_id(&head) else {
            return Ok(None);
        };
        let Some((Some(size), sl)) = read_size(&head[il..]) else {
            return Ok(None);
        };
        let segment_header = (il + sl) as u64 + size;
        let head = file.read_at(segment_header, 16).await?;
        let Some((SEGMENT, il)) = read_id(&head) else {
            return Ok(None);
        };
        let Some((_, sl)) = read_size(&head[il..]) else {
            return Ok(None);
        };
        let segment_data = segment_header + (il + sl) as u64;

        let mut scale: u64 = 1_000_000;
        let mut video_track: Option<u64> = None;
        let mut cues_at: Option<u64> = None;
        let mut info_at: Option<u64> = None;
        let mut tracks_at: Option<u64> = None;

        // Walk the top-level elements until the first cluster; the metadata
        // (and usually a SeekHead pointing at the cues) comes before it.
        let mut pos = segment_data;
        while pos < file.len {
            let head = file.read_at(pos, 16).await?;
            let Some((id, il)) = read_id(&head) else {
                break;
            };
            let Some((size, sl)) = read_size(&head[il..]) else {
                break;
            };
            let body_at = pos + (il + sl) as u64;
            let Some(size) = size else { break };
            match id {
                CLUSTER => break,
                SEEK_HEAD | INFO | TRACKS | CUES if size <= MAX_INDEX_BYTES => {
                    let body = file.read_at(body_at, size as usize).await?;
                    match id {
                        SEEK_HEAD => {
                            for (_, seek) in children(&body).filter(|(id, _)| *id == SEEK) {
                                let mut target = None;
                                let mut position = None;
                                for (id, v) in children(seek) {
                                    match id {
                                        SEEK_ID => target = read_id(v).map(|(id, _)| id),
                                        SEEK_POSITION => position = Some(read_uint(v)),
                                        _ => {}
                                    }
                                }
                                let at = position.map(|p| segment_data + p);
                                match target {
                                    Some(CUES) => cues_at = cues_at.or(at),
                                    Some(INFO) => info_at = info_at.or(at),
                                    Some(TRACKS) => tracks_at = tracks_at.or(at),
                                    _ => {}
                                }
                            }
                        }
                        INFO => scale = parse_scale(&body).unwrap_or(scale),
                        TRACKS => video_track = parse_video_track(&body),
                        CUES => {
                            return Ok(video_track.map(|t| parse_cues(&body, t, scale)));
                        }
                        _ => unreachable!(),
                    }
                }
                _ => {}
            }
            pos = body_at + size;
        }

        if video_track.is_none()
            && let Some(at) = tracks_at
            && let Some(body) = element_body(file, at, TRACKS).await?
        {
            video_track = parse_video_track(&body);
        }
        if let Some(at) = info_at
            && let Some(body) = element_body(file, at, INFO).await?
        {
            scale = parse_scale(&body).unwrap_or(scale);
        }
        let (Some(track), Some(at)) = (video_track, cues_at) else {
            return Ok(None);
        };
        let Some(body) = element_body(file, at, CUES).await? else {
            return Ok(None);
        };
        Ok(Some(parse_cues(&body, track, scale)))
    }

    async fn element_body(file: &mut File, at: u64, want: u32) -> std::io::Result<Option<Vec<u8>>> {
        let head = file.read_at(at, 16).await?;
        let Some((id, il)) = read_id(&head) else {
            return Ok(None);
        };
        let Some((Some(size), sl)) = read_size(&head[il..]) else {
            return Ok(None);
        };
        if id != want || size > MAX_INDEX_BYTES {
            return Ok(None);
        }
        Ok(Some(
            file.read_at(at + (il + sl) as u64, size as usize).await?,
        ))
    }

    fn parse_scale(info: &[u8]) -> Option<u64> {
        children(info)
            .find(|(id, _)| *id == TIMESTAMP_SCALE)
            .map(|(_, v)| read_uint(v))
            .filter(|&s| s > 0)
    }

    fn parse_video_track(tracks: &[u8]) -> Option<u64> {
        children(tracks)
            .filter(|(id, _)| *id == TRACK_ENTRY)
            .find_map(|(_, entry)| {
                let mut number = None;
                let mut kind = None;
                for (id, v) in children(entry) {
                    match id {
                        TRACK_NUMBER => number = Some(read_uint(v)),
                        TRACK_TYPE => kind = Some(read_uint(v)),
                        _ => {}
                    }
                }
                (kind == Some(1)).then_some(number).flatten()
            })
    }

    fn parse_cues(cues: &[u8], track: u64, scale: u64) -> Vec<u64> {
        children(cues)
            .filter(|(id, _)| *id == CUE_POINT)
            .filter_map(|(_, point)| {
                let mut time = None;
                let mut ours = false;
                for (id, v) in children(point) {
                    match id {
                        CUE_TIME => time = Some(read_uint(v)),
                        CUE_TRACK_POSITIONS => {
                            ours |=
                                children(v).any(|(id, v)| id == CUE_TRACK && read_uint(v) == track)
                        }
                        _ => {}
                    }
                }
                ours.then_some(time?.saturating_mul(scale))
            })
            .collect()
    }
}

mod mp4 {
    use super::{File, MAX_INDEX_BYTES};

    fn u32_at(b: &[u8], at: usize) -> Option<u32> {
        Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
    }

    fn u64_at(b: &[u8], at: usize) -> Option<u64> {
        Some(u64::from_be_bytes(b.get(at..at + 8)?.try_into().ok()?))
    }

    /// Child boxes of an in-memory box body.
    fn boxes(body: &[u8]) -> impl Iterator<Item = ([u8; 4], &[u8])> {
        let mut pos = 0usize;
        std::iter::from_fn(move || {
            let size = u32_at(body, pos)? as usize;
            let kind: [u8; 4] = body.get(pos + 4..pos + 8)?.try_into().ok()?;
            let (header, size) = match size {
                1 => (16, u64_at(body, pos + 8)? as usize),
                0 => (8, body.len() - pos),
                s => (8, s),
            };
            if size < header || pos + size > body.len() {
                return None;
            }
            let out = (kind, &body[pos + header..pos + size]);
            pos += size;
            Some(out)
        })
    }

    fn child<'a>(body: &'a [u8], kind: &[u8; 4]) -> Option<&'a [u8]> {
        boxes(body).find(|(k, _)| k == kind).map(|(_, b)| b)
    }

    pub(super) async fn keyframes(file: &mut File) -> std::io::Result<Option<Vec<u64>>> {
        // Find `moov` among the top-level boxes, reading headers only.
        let mut pos = 0u64;
        let moov = loop {
            if pos + 8 > file.len {
                return Ok(None);
            }
            let head = file.read_at(pos, 16).await?;
            let size = u32_at(&head, 0).unwrap_or(0) as u64;
            let kind = &head[4..8];
            let (header, size) = match size {
                1 => (16, u64_at(&head, 8).unwrap_or(0)),
                0 => (8, file.len - pos),
                s => (8, s),
            };
            if size < header {
                return Ok(None);
            }
            if kind == b"moov" {
                if size > MAX_INDEX_BYTES {
                    return Ok(None);
                }
                break file.read_at(pos + header, (size - header) as usize).await?;
            }
            if kind == b"moof" {
                // Fragmented: the index is spread over the file.
                return Ok(None);
            }
            pos += size;
        };

        Ok(boxes(&moov)
            .filter(|(k, _)| k == b"trak")
            .find_map(|(_, trak)| video_keyframes(trak)))
    }

    fn video_keyframes(trak: &[u8]) -> Option<Vec<u64>> {
        let mdia = child(trak, b"mdia")?;
        let hdlr = child(mdia, b"hdlr")?;
        if hdlr.get(8..12)? != b"vide" {
            return None;
        }
        let mdhd = child(mdia, b"mdhd")?;
        let timescale = if mdhd.first()? == &1 {
            u32_at(mdhd, 20)?
        } else {
            u32_at(mdhd, 12)?
        } as u64;
        if timescale == 0 {
            return None;
        }
        let stbl = child(child(mdia, b"minf")?, b"stbl")?;

        // Decode time of every sample.
        let stts = child(stbl, b"stts")?;
        let mut dts = Vec::new();
        let mut t: i64 = 0;
        for i in 0..u32_at(stts, 4)? as usize {
            let count = u32_at(stts, 8 + i * 8)?;
            let delta = u32_at(stts, 12 + i * 8)? as i64;
            for _ in 0..count {
                dts.push(t);
                t += delta;
            }
        }

        // Composition offsets, if any.
        let mut cts = vec![0i64; dts.len()];
        if let Some(ctts) = child(stbl, b"ctts") {
            let signed = ctts.first() == Some(&1);
            let mut sample = 0;
            for i in 0..u32_at(ctts, 4)? as usize {
                let count = u32_at(ctts, 8 + i * 8)?;
                let raw = u32_at(ctts, 12 + i * 8)?;
                let offset = if signed {
                    raw as i32 as i64
                } else {
                    raw as i64
                };
                for _ in 0..count {
                    if let Some(c) = cts.get_mut(sample) {
                        *c = offset;
                    }
                    sample += 1;
                }
            }
        }

        // The edit list shifts media time to presentation time.
        let shift = child(trak, b"edts")
            .and_then(|edts| child(edts, b"elst"))
            .and_then(|elst| {
                let v1 = elst.first() == Some(&1);
                (0..u32_at(elst, 4)? as usize).find_map(|i| {
                    let media_time = if v1 {
                        u64_at(elst, 16 + i * 20)? as i64
                    } else {
                        u32_at(elst, 12 + i * 12)? as i32 as i64
                    };
                    // -1 marks an empty edit (a delay), not a shift.
                    (media_time >= 0).then_some(media_time)
                })
            })
            .unwrap_or(0);

        let pts = |i: usize| -> u64 {
            let t = dts[i] + cts[i] - shift;
            (t.max(0) as u128 * 1_000_000_000 / timescale as u128) as u64
        };

        Some(match child(stbl, b"stss") {
            Some(stss) => (0..u32_at(stss, 4)? as usize)
                .filter_map(|i| u32_at(stss, 8 + i * 4))
                .filter_map(|n| (n as usize).checked_sub(1))
                .filter(|&i| i < dts.len())
                .map(pts)
                .collect(),
            // No sync sample table: every sample is a keyframe.
            None => (0..dts.len()).map(pts).collect(),
        })
    }
}
