//! HDR to SDR tone mapping, as a GStreamer element: 10-bit BT.2100 (PQ or
//! HLG) in, 8-bit BT.709 out.
//!
//! GStreamer has no tone mapper, and `videoconvert` can only remap transfer
//! functions, which clips everything brighter than SDR white. This keeps
//! luminance as mastered up to a knee and rolls the highlights above it off
//! into the remaining headroom (the BT.2390 approach), then converts the
//! BT.2020 gamut to BT.709.
//!
//! The per-pixel math is baked into a 3D lookup table over (Y, Cb, Cr) when
//! the caps are known, and frames are mapped with trilinear interpolation
//! on rayon's thread pool.

use std::sync::{LazyLock, Mutex};

use gst::glib;
use gst::prelude::*;
use gst::subclass::prelude::*;
use gst_base::subclass::prelude::*;
use gst_video::prelude::*;
use gst_video::subclass::prelude::*;
use rayon::prelude::*;

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

/// Lookup table grid points along luma, where the tone curve bends, and
/// along each chroma axis, where the mapping is smooth.
const GRID_Y: usize = 65;
const GRID_C: usize = 33;

/// Brightness that becomes SDR white: the reference white of BT.2408.
const REFERENCE_WHITE_NITS: f64 = 203.0;

/// Assumed peak brightness of PQ content without metadata saying otherwise.
const DEFAULT_PEAK_NITS: f64 = 1000.0;

/// The (Y, Cb, Cr) 10-bit → 8-bit mapping, sampled on a lattice. One table
/// per output channel, so the per-pixel luma lookups stay in cache.
struct Lut {
    channels: [Vec<f32>; 3],
}

impl Lut {
    fn new(hdr: Hdr, peak_nits: f64) -> Self {
        let mut channels: [Vec<f32>; 3] = Default::default();
        let code = |i: usize, n: usize| i as f64 * 1023.0 / (n - 1) as f64;
        for y in 0..GRID_Y {
            for cb in 0..GRID_C {
                for cr in 0..GRID_C {
                    let v = map_pixel(
                        hdr,
                        peak_nits,
                        code(y, GRID_Y),
                        code(cb, GRID_C),
                        code(cr, GRID_C),
                    );
                    for k in 0..3 {
                        channels[k].push(v[k]);
                    }
                }
            }
        }
        Self { channels }
    }

    #[inline]
    fn axis(v: u16, n: usize) -> (usize, f32) {
        let f = v.min(1023) as f32 * ((n - 1) as f32 / 1023.0);
        let i = (f as usize).min(n - 2);
        (i, f - i as f32)
    }

    /// The chroma half of a lattice position: shared by the four pixels of
    /// a 4:2:0 block.
    #[inline]
    fn locate_chroma(cb: u16, cr: u16) -> (usize, f32, f32) {
        let (bi, bf) = Self::axis(cb, GRID_C);
        let (ri, rf) = Self::axis(cr, GRID_C);
        (bi * GRID_C + ri, bf, rf)
    }

    /// Lattice offset of the cell holding (Y, chroma), and the position
    /// within it per axis.
    #[inline]
    fn locate(y: u16, chroma: (usize, f32, f32)) -> (usize, [f32; 3]) {
        let (yi, yf) = Self::axis(y, GRID_Y);
        (yi * GRID_C * GRID_C + chroma.0, [yf, chroma.1, chroma.2])
    }

    /// Trilinear interpolation of one output channel.
    #[inline]
    fn channel(&self, base: usize, frac: [f32; 3], k: usize) -> f32 {
        let t = &self.channels[k];
        let (dy, db) = (GRID_C * GRID_C, GRID_C);
        let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;
        let c00 = lerp(t[base], t[base + 1], frac[2]);
        let c01 = lerp(t[base + db], t[base + db + 1], frac[2]);
        let c10 = lerp(t[base + dy], t[base + dy + 1], frac[2]);
        let c11 = lerp(t[base + dy + db], t[base + dy + db + 1], frac[2]);
        lerp(lerp(c00, c01, frac[1]), lerp(c10, c11, frac[1]), frac[0])
    }

    /// Mapped luma of a 10-bit Y with a located chroma.
    #[inline]
    fn luma(&self, y: u16, chroma: (usize, f32, f32)) -> f32 {
        let (base, frac) = Self::locate(y, chroma);
        self.channel(base, frac, 0)
    }

    /// Mapped (Y, Cb, Cr) of a 10-bit (Y, Cb, Cr).
    #[cfg(test)]
    fn get(&self, y: u16, cb: u16, cr: u16) -> [f32; 3] {
        let (base, frac) = Self::locate(y, Self::locate_chroma(cb, cr));
        [0, 1, 2].map(|k| self.channel(base, frac, k))
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
        linear.map(|v| v * roll_off(luma, peak) / luma)
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

/// Where highlight compression starts, in linear light relative to
/// reference white. Everything darker is reproduced as mastered.
const KNEE: f64 = 0.6;

/// Maps linear luminance `x` (1.0 = reference white) into SDR range: the
/// identity up to `KNEE`, then an extended Reinhard curve that meets the
/// identity's slope at the knee and reaches 1.0 exactly at `peak`.
fn roll_off(x: f64, peak: f64) -> f64 {
    if x <= KNEE || peak <= 1.0 {
        return x.min(1.0);
    }
    let headroom = 1.0 - KNEE;
    let u = (x - KNEE) / headroom;
    let w = (peak - KNEE) / headroom;
    KNEE + headroom * (u * (1.0 + u / (w * w)) / (1.0 + u)).min(1.0)
}

glib::wrapper! {
    pub struct ToneMap(ObjectSubclass<imp::ToneMap>)
        @extends gst_video::VideoFilter, gst_base::BaseTransform, gst::Element, gst::Object;
}

impl ToneMap {
    pub(crate) fn element() -> gst::Element {
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
                s.remove_fields([
                    "colorimetry",
                    "chroma-site",
                    "mastering-display-info",
                    "content-light-level",
                ]);
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
            let lut = self
                .lut
                .lock()
                .unwrap()
                .clone()
                .ok_or(gst::FlowError::NotNegotiated)?;
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

            // One task per chroma row (two luma rows), on rayon's shared pool.
            let to_u8 = |v: f32| v.round().clamp(0.0, 255.0) as u8;
            let chroma_width = width.div_ceil(2);
            out_y
                .par_chunks_mut(out_strides[0] * 2)
                .zip(out_u.par_chunks_mut(out_strides[1]))
                .zip(out_v.par_chunks_mut(out_strides[2]))
                .take(height.div_ceil(2))
                .enumerate()
                .for_each(|(cy, ((oy, ou), ov))| {
                    for cx in 0..chroma_width {
                        let cb = read10(in_u, in_us, cx, cy);
                        let cr = read10(in_v, in_vs, cx, cy);
                        let chroma = Lut::locate_chroma(cb, cr);
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
                                oy[dy * out_strides[0] + x] = to_u8(lut.luma(luma, chroma));
                            }
                        }
                        // Chroma from the block's average luminance.
                        let (base, frac) = Lut::locate((sum / count.max(1)) as u16, chroma);
                        ou[cx] = to_u8(lut.channel(base, frac, 1));
                        ov[cx] = to_u8(lut.channel(base, frac, 2));
                    }
                });
            Ok(gst::FlowSuccess::Ok)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Nits to a 10-bit limited-range PQ luma code (the inverse EOTF).
    fn pq_code(nits: f64) -> f64 {
        const M1: f64 = 2610.0 / 16384.0;
        const M2: f64 = 2523.0 / 4096.0 * 128.0;
        const C1: f64 = 3424.0 / 4096.0;
        const C2: f64 = 2413.0 / 4096.0 * 32.0;
        const C3: f64 = 2392.0 / 4096.0 * 32.0;
        let y = (nits / 10000.0).powf(M1);
        64.0 + 876.0 * ((C1 + C2 * y) / (1.0 + C3 * y)).powf(M2)
    }

    #[test]
    fn black_stays_black_and_white_stays_white() {
        // Black.
        let black = map_pixel(Hdr::Pq, 1000.0, 64.0, 512.0, 512.0);
        assert!((black[0] - 16.0).abs() < 1.0, "{black:?}");
        // Reference white (203 nits is PQ code ~0.58) lands near SDR white,
        // just below it to leave room for highlights.
        let white = map_pixel(Hdr::Pq, 1000.0, pq_code(REFERENCE_WHITE_NITS), 512.0, 512.0);
        assert!(white[0] > 195.0 && white[0] < 230.0, "{white:?}");
        // Midtones are reproduced as mastered: 20% of reference white maps
        // to what BT.709 encodes 20% as.
        let mid = map_pixel(
            Hdr::Pq,
            1000.0,
            pq_code(0.2 * REFERENCE_WHITE_NITS),
            512.0,
            512.0,
        );
        let expected = 16.0 + 219.0 * bt709_oetf(0.2) as f32;
        assert!((mid[0] - expected).abs() < 3.0, "{mid:?} vs {expected}");
        // Neutral stays neutral.
        assert!(
            (white[1] - 128.0).abs() < 1.0 && (white[2] - 128.0).abs() < 1.0,
            "{white:?}"
        );
        // The peak maps to (about) full white, not beyond.
        let peak = map_pixel(Hdr::Pq, 1000.0, pq_code(1000.0), 512.0, 512.0);
        assert!(peak[0] > 225.0 && peak[0] <= 235.5, "{peak:?}");
    }

    #[test]
    fn roll_off_is_smooth_and_bounded() {
        assert_eq!(roll_off(0.3, 4.9), 0.3);
        let below = roll_off(KNEE - 1e-6, 4.9);
        let above = roll_off(KNEE + 1e-6, 4.9);
        assert!((above - below).abs() < 1e-5);
        assert!((roll_off(4.9, 4.9) - 1.0).abs() < 1e-9);
        assert!(roll_off(10.0, 4.9) <= 1.0);
        let mut last = 0.0;
        for i in 0..100 {
            let y = roll_off(i as f64 * 0.05, 4.9);
            assert!(y >= last);
            last = y;
        }
    }

    #[test]
    fn lut_matches_direct_math() {
        let lut = Lut::new(Hdr::Pq, 1000.0);
        for (y, cb, cr) in [(300u16, 400u16, 600u16), (700, 512, 512), (64, 512, 512)] {
            let direct = map_pixel(Hdr::Pq, 1000.0, y as f64, cb as f64, cr as f64);
            let interp = lut.get(y, cb, cr);
            for k in 0..3 {
                assert!(
                    (direct[k] - interp[k]).abs() < 3.0,
                    "{direct:?} vs {interp:?}"
                );
            }
        }
    }

    /// Throughput of the element alone: `cargo test -p media --release --
    /// --ignored --nocapture tonemap_speed`.
    #[test]
    #[ignore]
    fn tonemap_speed() {
        crate::init().unwrap();
        for (w, h) in [(1920, 1080), (3840, 2160)] {
            let frames = 96;
            let pipeline = gst::Pipeline::new();
            // One frame, repeated: measures the tone mapper, not the source.
            let src = gst::ElementFactory::make("videotestsrc")
                .property("num-buffers", 1)
                .build()
                .unwrap();
            let caps = crate::pipeline::capsfilter(&format!(
                "video/x-raw,format=I420_10LE,width={w},height={h},framerate=24/1,colorimetry=bt2100-pq"
            ))
            .unwrap();
            let freeze = gst::ElementFactory::make("imagefreeze")
                .property("num-buffers", frames)
                .build()
                .unwrap();
            let sink = gst::ElementFactory::make("fakesink")
                .property("sync", false)
                .build()
                .unwrap();
            let tonemap = ToneMap::element();
            pipeline
                .add_many([&src, &caps, &freeze, &tonemap, &sink])
                .unwrap();
            gst::Element::link_many([&src, &caps, &freeze, &tonemap, &sink]).unwrap();
            let start = std::time::Instant::now();
            pipeline.set_state(gst::State::Playing).unwrap();
            let msg = pipeline
                .bus()
                .unwrap()
                .timed_pop_filtered(
                    gst::ClockTime::from_seconds(120),
                    &[gst::MessageType::Eos, gst::MessageType::Error],
                )
                .unwrap();
            pipeline.set_state(gst::State::Null).unwrap();
            assert_eq!(msg.type_(), gst::MessageType::Eos, "{msg:?}");
            let fps = frames as f64 / start.elapsed().as_secs_f64();
            println!(
                "{w}x{h}: {fps:.0} fps ({:.1}x real time at 24 fps)",
                fps / 24.0
            );
        }
    }

    /// Round trip: an SDR frame converted to HDR10 mathematically, then tone
    /// mapped back, should match the original except in the highlights.
    /// `TONEMAP_FRAME=<video> TONEMAP_OUT=<dir> cargo test -p media --release
    /// -- --ignored --nocapture tonemap_round_trip` writes both as PNGs.
    #[test]
    #[ignore]
    fn tonemap_round_trip() {
        use gst_app::AppSink;
        crate::init().unwrap();
        let path = std::env::var("TONEMAP_FRAME").expect("TONEMAP_FRAME");
        let out = std::path::PathBuf::from(std::env::var("TONEMAP_OUT").expect("TONEMAP_OUT"));
        let (w, h) = (640usize, 360usize);

        // One RGB frame from 20s in.
        let pipeline = gst::parse::launch(&format!(
            "filesrc location=\"{path}\" ! decodebin force-sw-decoders=true ! videoconvert ! videoscale ! video/x-raw,format=RGB,width={w},height={h},pixel-aspect-ratio=1/1 ! appsink name=sink sync=false"
        ))
        .unwrap()
        .downcast::<gst::Pipeline>()
        .unwrap();
        let sink = pipeline
            .by_name("sink")
            .unwrap()
            .downcast::<AppSink>()
            .unwrap();
        pipeline.set_state(gst::State::Paused).unwrap();
        pipeline.state(gst::ClockTime::from_seconds(10)).0.unwrap();
        pipeline
            .seek_simple(
                gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
                gst::ClockTime::from_seconds(20),
            )
            .unwrap();
        pipeline.set_state(gst::State::Playing).unwrap();
        let sample = sink
            .try_pull_sample(gst::ClockTime::from_seconds(10))
            .unwrap();
        let info = gst_video::VideoInfo::from_caps(sample.caps().unwrap()).unwrap();
        let stride = info.stride()[0] as usize;
        let map = sample.buffer().unwrap().map_readable().unwrap();
        let original: Vec<[f64; 3]> = (0..h)
            .flat_map(|y| (0..w).map(move |x| (x, y)))
            .map(|(x, y)| {
                let i = y * stride + x * 3;
                [map[i], map[i + 1], map[i + 2]].map(|v| v as f64 / 255.0)
            })
            .collect();
        pipeline.set_state(gst::State::Null).unwrap();

        // SDR (BT.709) to HDR10: linear light, reference white at 203 nits,
        // BT.2020 primaries, PQ, Y'CbCr 10-bit.
        let inverse_709 = |v: f64| {
            if v < 0.081 {
                v / 4.5
            } else {
                ((v + 0.099) / 1.099).powf(1.0 / 0.45)
            }
        };
        let pq = |nits: f64| (pq_code(nits) - 64.0) / 876.0;
        let lut = Lut::new(Hdr::Pq, 1000.0);
        let mut mapped_rgb = Vec::with_capacity(w * h);
        let mut error = 0.0;
        for rgb in &original {
            let [r, g, b] = rgb.map(inverse_709);
            let rgb2020 = [
                0.6274 * r + 0.3293 * g + 0.0433 * b,
                0.0691 * r + 0.9195 * g + 0.0114 * b,
                0.0164 * r + 0.0880 * g + 0.8956 * b,
            ]
            .map(|v| pq(v * REFERENCE_WHITE_NITS));
            let y = 0.2627 * rgb2020[0] + 0.6780 * rgb2020[1] + 0.0593 * rgb2020[2];
            let cb = (rgb2020[2] - y) / 1.8814;
            let cr = (rgb2020[0] - y) / 1.4746;
            let code = |v: f64, off: f64, range: f64| (off + range * v).round() as u16;
            let [y8, cb8, cr8] = lut.get(
                code(y, 64.0, 876.0),
                code(cb, 512.0, 896.0),
                code(cr, 512.0, 896.0),
            );
            // Back to BT.709 R'G'B' for comparison.
            let (y, cb, cr) = (
                (y8 as f64 - 16.0) / 219.0,
                (cb8 as f64 - 128.0) / 224.0,
                (cr8 as f64 - 128.0) / 224.0,
            );
            let out = [
                y + 1.5748 * cr,
                y - 0.1873 * cb - 0.4681 * cr,
                y + 1.8556 * cb,
            ]
            .map(|v| v.clamp(0.0, 1.0));
            error += (0..3).map(|k| (out[k] - rgb[k]).abs()).sum::<f64>() / 3.0;
            mapped_rgb.push(out);
        }
        println!(
            "mean absolute difference: {:.1}%",
            100.0 * error / original.len() as f64
        );

        // Write both frames as PNGs.
        for (name, frame) in [("original.png", &original), ("tonemapped.png", &mapped_rgb)] {
            let bytes: Vec<u8> = frame
                .iter()
                .flat_map(|p| p.map(|v| (v * 255.0).round() as u8))
                .collect();
            let pipeline = gst::parse::launch(&format!(
                "appsrc name=src caps=video/x-raw,format=RGB,width={w},height={h},framerate=1/1 ! videoconvert ! pngenc ! filesink location=\"{}\"",
                out.join(name).display()
            ))
            .unwrap()
            .downcast::<gst::Pipeline>()
            .unwrap();
            let src = pipeline
                .by_name("src")
                .unwrap()
                .downcast::<gst_app::AppSrc>()
                .unwrap();
            pipeline.set_state(gst::State::Playing).unwrap();
            src.push_buffer(gst::Buffer::from_mut_slice(bytes)).unwrap();
            src.end_of_stream().unwrap();
            pipeline
                .bus()
                .unwrap()
                .timed_pop_filtered(gst::ClockTime::from_seconds(10), &[gst::MessageType::Eos])
                .unwrap();
            pipeline.set_state(gst::State::Null).unwrap();
        }
    }
}
