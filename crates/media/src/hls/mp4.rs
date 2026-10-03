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

/// Start of a media fragment in nanoseconds, from its `tfdt`.
pub(super) fn start_ns(fragment: &[u8], timescale: u32) -> Option<u64> {
    let tfdt = find(fragment, b"tfdt")?;
    let decode_time = if *tfdt.first()? == 1 {
        u64::from_be_bytes(tfdt.get(4..12)?.try_into().ok()?)
    } else {
        u32::from_be_bytes(tfdt.get(4..8)?.try_into().ok()?) as u64
    };
    Some((decode_time as u128 * 1_000_000_000 / timescale as u128) as u64)
}
