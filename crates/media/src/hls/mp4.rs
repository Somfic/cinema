//! Just enough ISO BMFF reading to place a fragment on the timeline.

fn find<'a>(data: &'a [u8], name: &[u8; 4]) -> Option<&'a [u8]> {
    let mut i = 0;
    while i + 8 <= data.len() {
        let size = u32::from_be_bytes(data[i..i + 4].try_into().ok()?) as usize;
        if size < 8 || i + size > data.len() {
            return None;
        }
        let kind = &data[i + 4..i + 8];
        let body = &data[i + 8..i + size];
        if kind == name {
            return Some(body);
        }
        if matches!(kind, b"moof" | b"traf" | b"moov" | b"trak" | b"mdia")
            && let Some(found) = find(body, name)
        {
            return Some(found);
        }
        i += size;
    }
    None
}

/// Track timescale from an init segment.
pub(super) fn timescale(init: &[u8]) -> Option<u32> {
    let mdhd = find(init, b"mdhd")?;
    let at = if *mdhd.first()? == 1 { 20 } else { 12 };
    Some(u32::from_be_bytes(mdhd.get(at..at + 4)?.try_into().ok()?)).filter(|&t| t > 0)
}

/// Display time of a media fragment's first frame, in nanoseconds: its
/// decode time (`tfdt`) plus the first sample's composition offset. With
/// reordered frames the two differ - in open-GOP HEVC the keyframe decodes
/// a few frames before it is shown - and segments are cut on display time.
pub(super) fn start_ns(fragment: &[u8], timescale: u32) -> Option<u64> {
    let tfdt = find(fragment, b"tfdt")?;
    let decode_time = if *tfdt.first()? == 1 {
        u64::from_be_bytes(tfdt.get(4..12)?.try_into().ok()?)
    } else {
        u32::from_be_bytes(tfdt.get(4..8)?.try_into().ok()?) as u64
    } as i128;
    let start = decode_time + first_composition_offset(fragment).unwrap_or(0) as i128;
    Some((start.max(0) as u128 * 1_000_000_000 / timescale as u128) as u64)
}

/// The first sample's composition offset from `trun`, if it carries them.
fn first_composition_offset(fragment: &[u8]) -> Option<i64> {
    let trun = find(fragment, b"trun")?;
    let version = *trun.first()?;
    let flags = u32::from_be_bytes(trun.get(0..4)?.try_into().ok()?) & 0x00FF_FFFF;
    if flags & 0x800 == 0 {
        return None;
    }
    // version/flags, sample count, then the optional fields before the
    // per-sample entries.
    let mut at = 8;
    if flags & 0x1 != 0 {
        at += 4; // data offset
    }
    if flags & 0x4 != 0 {
        at += 4; // first sample flags
    }
    // Within the first sample's entry: duration, size, flags, then the offset.
    for bit in [0x100, 0x200, 0x400] {
        if flags & bit != 0 {
            at += 4;
        }
    }
    let raw = u32::from_be_bytes(trun.get(at..at + 4)?.try_into().ok()?);
    Some(if version == 1 {
        raw as i32 as i64
    } else {
        raw as i64
    })
}
