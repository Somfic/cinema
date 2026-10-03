//! HDR to SDR tone mapping, as a GStreamer element: 10-bit BT.2100 (PQ or
//! HLG) in, 8-bit BT.709 out.
//!
//! GStreamer has no tone mapper, and `videoconvert` can only remap transfer
//! functions, which clips everything brighter than SDR white. This maps
//! luminance through a filmic (Hable) curve instead, so highlights roll off,
//! then converts the BT.2020 gamut to BT.709.
//!
//! The per-pixel math is baked into a 3D lookup table over (Y, Cb, Cr) when
//! the caps are known, and frames are mapped with trilinear interpolation
//! across a few threads: fast enough to keep ahead of live playback at
//! 1080p.

use std::sync::{LazyLock, Mutex};

use gst::glib;
use gst::prelude::*;
use gst::subclass::prelude::*;
use gst_base::subclass::prelude::*;
use gst_video::prelude::*;
use gst_video::subclass::prelude::*;

/// The HDR transfer function of a source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hdr {
    /// SMPTE ST 2084 (HDR10, HDR10+, Dolby Vision with an HDR10 base).
    Pq,
    /// ARIB STD-B67 (broadcast HDR).
    Hlg,
}

impl Hdr {
    /// The HDR transfer a caps' colorimetry declares, if any.
    pub(crate) fn from_caps(caps: &gst::CapsRef) -> Option<Hdr> {
        let colorimetry: gst_video::VideoColorimetry = caps
            .structure(0)?
            .get::<String>("colorimetry")
            .ok()?
            .parse()
            .ok()?;
        match colorimetry.transfer() {
            gst_video::VideoTransferFunction::Smpte2084 => Some(Hdr::Pq),
            gst_video::VideoTransferFunction::AribStdB67 => Some(Hdr::Hlg),
            _ => None,
        }
    }
}

/// Grid points per axis of the lookup table.
const GRID: usize = 33;

/// Brightness that becomes SDR white: the reference white of BT.2408.
const REFERENCE_WHITE_NITS: f64 = 203.0;

/// Assumed peak brightness of PQ content without metadata saying otherwise.
const DEFAULT_PEAK_NITS: f64 = 1000.0;

/// The (Y, Cb, Cr) 10-bit → 8-bit mapping, sampled on a `GRID`³ lattice.
struct Lut {
    table: Vec<[f32; 3]>,
}

impl Lut {
    fn new(hdr: Hdr, peak_nits: f64) -> Self {
        let mut table = Vec::with_capacity(GRID * GRID * GRID);
        for y in 0..GRID {
            for cb in 0..GRID {
                for cr in 0..GRID {
                    let code = |i: usize| i as f64 * 1023.0 / (GRID - 1) as f64;
                    table.push(map_pixel(hdr, peak_nits, code(y), code(cb), code(cr)));
                }
            }
        }
        Self { table }
    }

    /// Trilinear lookup of 10-bit (Y, Cb, Cr).
    #[inline]
    fn get(&self, y: u16, cb: u16, cr: u16) -> [f32; 3] {
        let scale = (GRID - 1) as f32 / 1023.0;
        let pos = |v: u16| {
            let f = v.min(1023) as f32 * scale;
            let i = (f as usize).min(GRID - 2);
            (i, f - i as f32)
        };
        let ((yi, yf), (bi, bf), (ri, rf)) = (pos(y), pos(cb), pos(cr));
        let at = |a: usize, b: usize, c: usize| &self.table[(a * GRID + b) * GRID + c];
        let mut out = [0f32; 3];
        for (k, o) in out.iter_mut().enumerate() {
            let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;
            let c00 = lerp(at(yi, bi, ri)[k], at(yi, bi, ri + 1)[k], rf);
            let c01 = lerp(at(yi, bi + 1, ri)[k], at(yi, bi + 1, ri + 1)[k], rf);
            let c10 = lerp(at(yi + 1, bi, ri)[k], at(yi + 1, bi, ri + 1)[k], rf);
            let c11 = lerp(at(yi + 1, bi + 1, ri)[k], at(yi + 1, bi + 1, ri + 1)[k], rf);
            *o = lerp(lerp(c00, c01, bf), lerp(c10, c11, bf), yf);
        }
        out
    }
}

/// One limited-range 10-bit BT.2100 pixel to limited-range 8-bit BT.709.
fn map_pixel(hdr: Hdr, peak_nits: f64, y: f64, cb: f64, cr: f64) -> [f32; 3] {
    // Limited range to normalised non-linear values.
    let y = ((y - 64.0) / 876.0).clamp(0.0, 1.0);
    let cb = ((cb - 512.0) / 896.0).clamp(-0.5, 0.5);
    let cr = ((cr - 512.0) / 896.0).clamp(-0.5, 0.5);

    // BT.2020 non-constant-luminance Y'CbCr to R'G'B'.
    let rgb = [
        y + 1.4746 * cr,
        y - 0.164_553 * cb - 0.571_353 * cr,
        y + 1.8814 * cb,
    ]
    .map(|v| v.clamp(0.0, 1.0));

    // To linear light, relative to reference white.
    let linear = match hdr {
        Hdr::Pq => rgb.map(|v| pq_eotf(v) / REFERENCE_WHITE_NITS),
        Hdr::Hlg => {
            // Scene light, then the reference OOTF for a 1000-nit display.
            let scene = rgb.map(hlg_inverse_oetf);
            let ys = 0.2627 * scene[0] + 0.6780 * scene[1] + 0.0593 * scene[2];
            let gain = 1000.0 * ys.max(1e-6).powf(0.2) / REFERENCE_WHITE_NITS;
            scene.map(|v| v * gain)
        }
    };

    // Tone map luminance, scaling the channels with it to keep hues.
    let peak = match hdr {
        Hdr::Pq => peak_nits,
        Hdr::Hlg => 1000.0,
    } / REFERENCE_WHITE_NITS;
    let luma = 0.2627 * linear[0] + 0.6780 * linear[1] + 0.0593 * linear[2];
    let mapped = if luma > 0.0 {
        let target = hable(luma) / hable(peak.max(1.0));
        linear.map(|v| v * target / luma)
    } else {
        [0.0; 3]
    };

    // BT.2020 to BT.709 primaries; out-of-gamut colours clip.
    let [r, g, b] = mapped;
    let rgb709 = [
        1.6605 * r - 0.5876 * g - 0.0728 * b,
        -0.1246 * r + 1.1329 * g - 0.0083 * b,
        -0.0182 * r - 0.1006 * g + 1.1187 * b,
    ]
    .map(|v| bt709_oetf(v.clamp(0.0, 1.0)));

    // BT.709 R'G'B' to limited-range 8-bit Y'CbCr.
    let [r, g, b] = rgb709;
    let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    let cb = (b - y) / 1.8556;
    let cr = (r - y) / 1.5748;
    [
        (16.0 + 219.0 * y) as f32,
        (128.0 + 224.0 * cb) as f32,
        (128.0 + 224.0 * cr) as f32,
    ]
}

/// SMPTE ST 2084 EOTF: non-linear signal to nits.
fn pq_eotf(e: f64) -> f64 {
    const M1: f64 = 2610.0 / 16384.0;
    const M2: f64 = 2523.0 / 4096.0 * 128.0;
    const C1: f64 = 3424.0 / 4096.0;
    const C2: f64 = 2413.0 / 4096.0 * 32.0;
    const C3: f64 = 2392.0 / 4096.0 * 32.0;
    let p = e.powf(1.0 / M2);
    10000.0 * ((p - C1).max(0.0) / (C2 - C3 * p)).powf(1.0 / M1)
}

/// ARIB STD-B67 inverse OETF: non-linear signal to scene light (0..1).
fn hlg_inverse_oetf(e: f64) -> f64 {
    const A: f64 = 0.178_832_77;
    const B: f64 = 0.284_668_92;
    const C: f64 = 0.559_910_73;
    if e <= 0.5 {
        e * e / 3.0
    } else {
        (((e - C) / A).exp() + B) / 12.0
    }
}

fn bt709_oetf(l: f64) -> f64 {
    if l < 0.018 {
        4.5 * l
    } else {
        1.099 * l.powf(0.45) - 0.099
    }
}

/// John Hable's filmic curve (Uncharted 2).
fn hable(x: f64) -> f64 {
    const A: f64 = 0.15;
    const B: f64 = 0.50;
    const C: f64 = 0.10;
    const D: f64 = 0.20;
    const E: f64 = 0.02;
    const F: f64 = 0.30;
    ((x * (A * x + C * B) + D * E) / (x * (A * x + B) + D * F)) - E / F
}

glib::wrapper! {
    pub struct ToneMap(ObjectSubclass<imp::ToneMap>)
        @extends gst_video::VideoFilter, gst_base::BaseTransform, gst::Element, gst::Object;
}

impl ToneMap {
    pub(crate) fn new() -> gst::Element {
        glib::Object::new::<ToneMap>().upcast()
    }
}

mod imp {
    use super::*;

    static CAT: LazyLock<gst::DebugCategory> = LazyLock::new(|| {
        gst::DebugCategory::new(
            "cinematonemap",
            gst::DebugColorFlags::empty(),
            Some("HDR to SDR tone mapping"),
        )
    });

    #[derive(Default)]
    pub struct ToneMap {
        lut: Mutex<Option<std::sync::Arc<Lut>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ToneMap {
        const NAME: &'static str = "CinemaToneMap";
        type Type = super::ToneMap;
        type ParentType = gst_video::VideoFilter;
    }

    impl ObjectImpl for ToneMap {}
    impl GstObjectImpl for ToneMap {}

    impl ElementImpl for ToneMap {
        fn metadata() -> Option<&'static gst::subclass::ElementMetadata> {
            static META: LazyLock<gst::subclass::ElementMetadata> = LazyLock::new(|| {
                gst::subclass::ElementMetadata::new(
                    "Cinema tone map",
                    "Filter/Converter/Video",
                    "Tone maps 10-bit BT.2100 HDR to 8-bit BT.709 SDR",
                    "cinema",
                )
            });
            Some(&*META)
        }

        fn pad_templates() -> &'static [gst::PadTemplate] {
            static TEMPLATES: LazyLock<Vec<gst::PadTemplate>> = LazyLock::new(|| {
                let sink = gst_video::VideoCapsBuilder::new()
                    .format(gst_video::VideoFormat::I42010le)
                    .build();
                let src = gst_video::VideoCapsBuilder::new()
                    .format(gst_video::VideoFormat::I420)
                    .build();
                vec![
                    gst::PadTemplate::new(
                        "sink",
                        gst::PadDirection::Sink,
                        gst::PadPresence::Always,
                        &sink,
                    )
                    .unwrap(),
                    gst::PadTemplate::new(
                        "src",
                        gst::PadDirection::Src,
                        gst::PadPresence::Always,
                        &src,
                    )
                    .unwrap(),
                ]
            });
            TEMPLATES.as_ref()
        }
    }

    impl BaseTransformImpl for ToneMap {
        const MODE: gst_base::subclass::BaseTransformMode =
            gst_base::subclass::BaseTransformMode::NeverInPlace;
        const PASSTHROUGH_ON_SAME_CAPS: bool = false;
        const TRANSFORM_IP_ON_PASSTHROUGH: bool = false;

        fn transform_caps(
            &self,
            direction: gst::PadDirection,
            caps: &gst::Caps,
            filter: Option<&gst::Caps>,
        ) -> Option<gst::Caps> {
            // Same geometry either way; only format and colorimetry change.
            let mut other = caps.clone();
            for s in other.make_mut().iter_mut() {
                s.remove_fields(["colorimetry", "chroma-site", "mastering-display-info", "content-light-level"]);
                match direction {
                    gst::PadDirection::Sink => {
                        s.set("format", gst_video::VideoFormat::I420.to_str());
                        s.set("colorimetry", "bt709");
                    }
                    _ => s.set("format", gst_video::VideoFormat::I42010le.to_str()),
                }
            }
            Some(match filter {
                Some(filter) => filter.intersect_with_mode(&other, gst::CapsIntersectMode::First),
                None => other,
            })
        }
    }

    impl VideoFilterImpl for ToneMap {
        fn set_info(
            &self,
            incaps: &gst::Caps,
            _in_info: &gst_video::VideoInfo,
            _outcaps: &gst::Caps,
            _out_info: &gst_video::VideoInfo,
        ) -> Result<(), gst::LoggableError> {
            let hdr = super::Hdr::from_caps(incaps).unwrap_or(super::Hdr::Pq);
            // MaxCLL, when the stream declares it, is the real peak.
            let peak = incaps
                .structure(0)
                .and_then(|s| s.get::<String>("content-light-level").ok())
                .and_then(|cll| cll.split(':').next()?.parse::<f64>().ok())
                .filter(|&nits| nits >= REFERENCE_WHITE_NITS)
                .unwrap_or(DEFAULT_PEAK_NITS);
            gst::debug!(CAT, imp = self, "Tone mapping {hdr:?}, peak {peak} nits");
            *self.lut.lock().unwrap() = Some(std::sync::Arc::new(Lut::new(hdr, peak)));
            Ok(())
        }

        fn transform_frame(
            &self,
            input: &gst_video::VideoFrameRef<&gst::BufferRef>,
            output: &mut gst_video::VideoFrameRef<&mut gst::BufferRef>,
        ) -> Result<gst::FlowSuccess, gst::FlowError> {
            let lut = self.lut.lock().unwrap().clone().ok_or(gst::FlowError::NotNegotiated)?;
            let (width, height) = (input.width() as usize, input.height() as usize);
            let in_data = |p: u32| input.plane_data(p).map_err(|_| gst::FlowError::Error);
            let in_stride = |p: usize| input.plane_stride()[p] as usize;
            let (in_y, in_ys) = (in_data(0)?, in_stride(0));
            let (in_u, in_us) = (in_data(1)?, in_stride(1));
            let (in_v, in_vs) = (in_data(2)?, in_stride(2));
            let out_strides = [
                output.plane_stride()[0] as usize,
                output.plane_stride()[1] as usize,
                output.plane_stride()[2] as usize,
            ];
            // Planes are separate memory; split the output borrow per plane.
            let [out_y, out_u, out_v, _] = output.planes_data_mut();

            let read10 = |data: &[u8], stride: usize, x: usize, y: usize| -> u16 {
                let i = y * stride + x * 2;
                u16::from_le_bytes([data[i], data[i + 1]])
            };

            // Work in bands of chroma rows (two luma rows each), one per thread.
            let chroma_rows = height.div_ceil(2);
            let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).min(8);
            let band = chroma_rows.div_ceil(threads).max(1);
            let to_u8 = |v: f32| v.round().clamp(0.0, 255.0) as u8;

            let y_bands = out_y.chunks_mut(out_strides[0] * band * 2);
            let u_bands = out_u.chunks_mut(out_strides[1] * band);
            let v_bands = out_v.chunks_mut(out_strides[2] * band);
            std::thread::scope(|scope| {
                for (n, ((oy, ou), ov)) in y_bands.zip(u_bands).zip(v_bands).enumerate() {
                    let lut = &lut;
                    scope.spawn(move || {
                        let first = n * band;
                        let last = ((n + 1) * band).min(chroma_rows);
                        for cy in first..last {
                            for cx in 0..width.div_ceil(2) {
                                let cb = read10(in_u, in_us, cx, cy);
                                let cr = read10(in_v, in_vs, cx, cy);
                                let mut sum = 0u32;
                                let mut count = 0u32;
                                for dy in 0..2 {
                                    let y = cy * 2 + dy;
                                    if y >= height {
                                        continue;
                                    }
                                    for dx in 0..2 {
                                        let x = cx * 2 + dx;
                                        if x >= width {
                                            continue;
                                        }
                                        let luma = read10(in_y, in_ys, x, y);
                                        sum += luma as u32;
                                        count += 1;
                                        let row = (y - first * 2) * out_strides[0];
                                        oy[row + x] = to_u8(lut.get(luma, cb, cr)[0]);
                                    }
                                }
                                // Chroma from the block's average luminance.
                                let mapped = lut.get((sum / count.max(1)) as u16, cb, cr);
                                let row = cy - first;
                                ou[row * out_strides[1] + cx] = to_u8(mapped[1]);
                                ov[row * out_strides[2] + cx] = to_u8(mapped[2]);
                            }
                        }
                    });
                }
            });
            Ok(gst::FlowSuccess::Ok)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn black_stays_black_and_white_stays_white() {
        // Black.
        let black = map_pixel(Hdr::Pq, 1000.0, 64.0, 512.0, 512.0);
        assert!((black[0] - 16.0).abs() < 1.0, "{black:?}");
        // Reference white (203 nits is PQ code ~0.58) lands near SDR white
        // but leaves headroom for highlights.
        let white_code = 64.0 + 876.0 * 0.5806;
        let white = map_pixel(Hdr::Pq, 1000.0, white_code, 512.0, 512.0);
        assert!(white[0] > 150.0 && white[0] < 235.0, "{white:?}");
        // Neutral stays neutral.
        assert!((white[1] - 128.0).abs() < 1.0 && (white[2] - 128.0).abs() < 1.0, "{white:?}");
        // The peak maps to (about) full white, not beyond.
        let peak = map_pixel(Hdr::Pq, 1000.0, 64.0 + 876.0 * 0.7518, 512.0, 512.0);
        assert!(peak[0] > 225.0 && peak[0] <= 235.5, "{peak:?}");
    }

    #[test]
    fn lut_matches_direct_math() {
        let lut = Lut::new(Hdr::Pq, 1000.0);
        for (y, cb, cr) in [(300u16, 400u16, 600u16), (700, 512, 512), (64, 512, 512)] {
            let direct = map_pixel(Hdr::Pq, 1000.0, y as f64, cb as f64, cr as f64);
            let interp = lut.get(y, cb, cr);
            for k in 0..3 {
                assert!((direct[k] - interp[k]).abs() < 3.0, "{direct:?} vs {interp:?}");
            }
        }
    }
}
