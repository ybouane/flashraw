//! Canon TIFF-based raws: CR2 (all generations incl. sRAW/mRAW) and the TIFF raws of the
//! EOS-1D / 1Ds / D2000.
//!
//! The sensor data is one lossless-JPEG stream. Opening a file means: reading the generic EXIF
//! metadata, the Canon maker note, choosing the stream, deriving the geometry of the buffer (from the
//! [`geometry`] table, a few per-model rules or the maker note's SensorInfo) and describing how to
//! decode it. Nothing is decoded before [`RawDecoder::decode`] runs.

use super::geometry::{self, Geometry};
use super::makernote::{CanonMakernote, black_pattern, entry_words};
use crate::common::*;
use crate::orientation;
use flashraw_codecs::canon_cr2::{self, Slices};
use flashraw_codecs::canon_sraw::{self, SrawConversion, SrawParams};
use flashraw_codecs::ljpeg::{LjpegHeader, LjpegOptions};
use flashraw_core::bytes::slice_at_lenient;
use flashraw_core::exif;
use flashraw_core::tiff::{Ifd, Tiff, tags};
use flashraw_core::*;

/// TIFF tags this reader looks at besides the standard ones.
const TAG_SLICES: u16 = 0xC640;
const TAG_LINEARISATION: u16 = 0x0123;
const TAG_KODAK_IFD: u16 = 0x8290;

/// Locate the maker note IFD (EXIF tag 0x927C; its offsets count from the start of the TIFF).
pub(super) fn makernote_ifd<'a>(tiff: &Tiff<'a>) -> Option<Ifd<'a>> {
    let entry = tiff.all_ifds().into_iter().find_map(|ifd| ifd.exif.as_ref()?.get(tags::MAKER_NOTE))?;
    tiff.sub_ifd_at(entry.value_field).ok()
}

// ---------------------------------------------------------------------------------------------
// The raw stream
// ---------------------------------------------------------------------------------------------

/// A lossless-JPEG stream that may carry the sensor data.
struct Stream<'a> {
    /// File offset of the first byte.
    offset: usize,
    bytes: &'a [u8],
    /// Length the container declares (0 when it declares none).
    declared: usize,
    hdr: LjpegHeader,
    slices: Slices,
    /// Linearisation curve (Kodak-built bodies).
    curve: Option<Vec<u16>>,
    /// Colour filter pattern recorded by TIFF-EP tags next to the stream.
    tiff_cfa: Option<CfaPattern>,
    /// Found through the EOS-1D maker-note pointer.
    from_makernote: bool,
}

/// Dimensions of the sensor buffer a lossless-JPEG frame describes. Interleaved components widen the
/// frame; a frame more than four times wider than tall holds two sensor rows per JPEG row.
fn frame_dims(hdr: &LjpegHeader) -> (u32, u32) {
    let (mut w, mut h) = (hdr.width, hdr.height);
    let even = hdr.clrs % 2 == 0;
    if hdr.sraw == 0 && even {
        w = w.saturating_mul(hdr.clrs);
    }
    if even && w > h.saturating_mul(4) {
        w /= 2;
        h = h.saturating_mul(2);
    }
    (w, h)
}

/// A candidate's bytes with its header, when they start a lossless-JPEG stream of sensor data.
fn accept(bytes: &[u8]) -> Option<LjpegHeader> {
    if bytes.len() < 4 || bytes[0] != 0xFF || bytes[1] != 0xD8 {
        return None;
    }
    let hdr = LjpegHeader::parse(bytes, LjpegOptions::default()).ok()?;
    // An RGB frame is a preview, not sensor data.
    (!(hdr.clrs == 3 && hdr.sraw == 0)).then_some(hdr)
}

fn find_stream<'a>(data: &'a [u8], tiff: &Tiff<'a>, mn: &CanonMakernote) -> Option<Stream<'a>> {
    let mut best: Option<(u64, Stream<'a>)> = None;
    // The first candidate of the best score wins.
    let mut consider = |stream: Stream<'a>| {
        let (w, h) = frame_dims(&stream.hdr);
        let score = w as u64 * h as u64 * stream.hdr.bits as u64;
        if best.as_ref().is_none_or(|(s, _)| score > *s) {
            best = Some((score, stream));
        }
    };
    for ifd in tiff.all_ifds() {
        let &[(offset, len)] = &strips(ifd)[..] else { continue };
        let bytes = if len > 0 { slice_at_lenient(data, offset, len).ok() } else { data.get(offset..) };
        let Some((bytes, hdr)) = bytes.and_then(|b| accept(b).map(|h| (b, h))) else { continue };
        let slices = ifd.get(TAG_SLICES).map(|e| Slices::from_tag(&e.u32s())).unwrap_or_default();
        let curve: Vec<u16> = ifd
            .get(TAG_LINEARISATION)
            .map(|e| e.u32s().into_iter().take(65536).map(|v| v.min(65535) as u16).collect())
            .unwrap_or_default();
        consider(Stream {
            offset,
            bytes,
            declared: len,
            hdr,
            slices,
            curve: (!curve.is_empty()).then_some(curve),
            tiff_cfa: tiff_ep_cfa(ifd),
            from_makernote: false,
        });
    }
    if let Some(offset) = mn.raw_1d.map(|o| o as usize) {
        if let Some((bytes, hdr)) = data.get(offset..).and_then(|b| accept(b).map(|h| (b, h))) {
            consider(Stream {
                offset,
                bytes,
                declared: 0,
                hdr,
                slices: Slices::default(),
                curve: None,
                tiff_cfa: None,
                from_makernote: true,
            });
        }
    }
    best.map(|(_, s)| s)
}

// ---------------------------------------------------------------------------------------------
// Kodak-built bodies (EOS D2000 / D6000): vendor IFD with black level and white balance
// ---------------------------------------------------------------------------------------------

/// Which white-balance preset of the vendor IFD applies.
#[derive(Clone, Copy, PartialEq)]
enum Preset {
    /// A preset slot 0..=5 (Daylight, Tungsten, Fluorescent, Flash, Custom, Auto).
    Slot(u16),
    /// The camera's own multipliers (tag 0x03FD) replace the presets.
    Software,
}

/// Black level and white balance (R, G, B, G2 multipliers) of a Kodak-built body.
#[derive(Default)]
struct KodakInfo {
    black: Option<u32>,
    wb: Option<[f32; 4]>,
}

/// Vendor tags of the Kodak IFD (unverified beyond the one corpus file).
fn kodak_info(tiff: &Tiff) -> KodakInfo {
    let mut out = KodakInfo::default();
    let Some(ifd) = tiff.ifds.first().and_then(|i| i.u32(TAG_KODAK_IFD)).and_then(|o| tiff.sub_ifd_at(o).ok()) else {
        return out;
    };
    let mut preset: Option<Preset> = None;
    for e in &ifd.entries {
        match e.tag {
            0x03EF => {
                if let Some(v) = e.u32(0) {
                    out.black = Some(v);
                }
            }
            0x03F0 => {
                if let Some(v) = e.u32(0) {
                    out.black = Some(match out.black {
                        Some(b) if b != 0 => ((b as u64 + v as u64) / 2) as u32,
                        _ => v,
                    });
                }
            }
            0x03FC => preset = e.u32(0).filter(|&v| v < 6).map(|v| Preset::Slot(v as u16)),
            0x03FD if e.count == 72 => {
                if let Some(m) = entry_words(e).get(20..23) {
                    let gain = |v: u16| 2048.0 / v.max(1) as f32;
                    out.wb = Some([gain(m[0]), gain(m[1]), gain(m[2]), gain(m[1])]);
                }
                preset = Some(Preset::Software);
            }
            0x0848..=0x084D => {
                let slot = e.tag - 0x0848;
                if e.tag == 0x084D && preset.is_none() {
                    preset = Some(Preset::Slot(5));
                }
                if preset == Some(Preset::Slot(slot)) {
                    let rgb = |i: usize| e.f64(i).map(|v| v as f32).filter(|&v| v > 0.001).unwrap_or(1.0);
                    let (r, g, b) = (rgb(0), rgb(1), rgb(2));
                    let g2 = g * g;
                    let trunc = |v: f32| (v as i32) as f32;
                    out.wb = Some([trunc(g2 / r), trunc(g), trunc(g2 / b), trunc(g)]);
                }
            }
            _ => {}
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Open
// ---------------------------------------------------------------------------------------------

/// Everything the maker note and the container say about a file, before the path split.
struct Parsed<'a> {
    meta: RawMetadata,
    mn: CanonMakernote,
    stream: Stream<'a>,
    orientation: Orientation,
}

/// Open a Canon TIFF-based raw (called through `super::open_tiff`).
pub fn open<'a>(data: &'a [u8], tiff: Tiff<'a>, opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    let ifd0 = tiff.ifds.first().ok_or_else(|| Error::corrupt("CR2 without IFD0"))?;
    let mut meta = RawMetadata::default();
    exif::read_ifd0(ifd0, &mut meta);

    let mn = makernote_ifd(&tiff).map(|ifd| CanonMakernote::parse(&ifd)).unwrap_or_default();
    if meta.lens.model.is_none() {
        meta.lens.model = mn.lens_model.clone();
    }
    if meta.lens.focal_range.is_none() {
        meta.lens.focal_range = mn.focal_range;
    }
    if meta.serial.is_none() {
        meta.serial = mn.serial.map(|s| s.to_string());
    }
    if meta.firmware.is_none() {
        meta.firmware = mn.firmware.clone();
    }
    if let Some(id) = mn.model_id {
        meta.set_extra("canon_model_id", format!("{id:#010x}"));
    }

    let stream = find_stream(data, &tiff, &mn).ok_or_else(|| Error::CameraUnsupported {
        make: meta.make.clone(),
        model: meta.model.clone(),
        reason: "no lossless-JPEG raw stream found".into(),
    })?;

    // Orientation: the standard tag, the camera's orientation sensor and its Auto Rotate setting.
    let cam = flashraw_camera_db::find(&meta.make, &meta.model, "");
    let layout = cam.and_then(|c| c.orientation);
    let (sensor, auto_rotate) =
        mn.camera_orientation(layout.and_then(|l| l.maker_note_offset), layout.and_then(|l| l.auto_rotate_offset));
    let policy = orientation::policy_for(&meta, cam);
    let exif_orientation = orientation::tiff_orientation(&tiff.ifds);
    let orientation = orientation::apply(&mut meta, exif_orientation, sensor, auto_rotate, policy);

    // Previews: every embedded image except the raw stream itself.
    let (fw, fh) = frame_dims(&stream.hdr);
    let mut previews = tiff_previews(data, 0, &tiff, orientation);
    previews.retain(|p| p.offset != stream.offset as u64);
    classify_previews(&mut previews);
    orientation::resolve_previews(data, &mut previews, orientation, fw.max(fh), fw.min(fh));
    meta.previews = previews;

    let parsed = Parsed { meta, mn, stream, orientation };
    if parsed.stream.hdr.sraw != 0 { open_sraw(data, parsed, opts) } else { open_bayer(data, &tiff, parsed, opts) }
}

/// Map a codec error to a truncation report when the stream runs past the end of the file.
fn truncation_mapper(file_len: usize, offset: usize, declared: usize) -> impl Fn(Error) -> Error {
    move |e| e.or_truncated(file_len, DataUnit::Strip(0), offset, declared)
}

// ---------------------------------------------------------------------------------------------
// Bayer files
// ---------------------------------------------------------------------------------------------

/// The ModelIDs of the PowerShot G11 (`0x02700000`) and S90 (`0x02720000`) differ by one bit.
fn is_g11_or_s90(model_id: u32) -> bool {
    model_id | 0x20000 == 0x0272_0000
}

/// Origin of the active area of the G11 / S90 inside their 3744 x 2784 buffer (verified on files).
const G11_ORIGIN: (u32, u32) = (8, 16);

/// The active area and optical-black rectangles of a Bayer buffer.
struct BayerArea {
    /// Active area before the even-origin rounding.
    area: Geometry,
    /// Column band parameters for the optical-black rectangles (`None`: use `fixed_black`).
    band: Option<geometry::BandParams>,
    /// Optical-black rectangles that replace the bands.
    fixed_black: Vec<Rect>,
    forced_cfa: Option<&'static str>,
    has_row: bool,
}

fn bayer_area(raw_w: u32, raw_h: u32, model: &str, mn: &CanonMakernote) -> BayerArea {
    let row = geometry::find(raw_w, raw_h);
    let mut a = BayerArea {
        area: row.map_or(Geometry { left: 0, top: 0, width: raw_w, height: raw_h }, |r| r.geometry()),
        band: row.map(|r| r.band),
        fixed_black: Vec::new(),
        forced_cfa: row.and_then(|r| r.forced_cfa),
        has_row: row.is_some(),
    };
    if mn.model_id.is_some_and(is_g11_or_s90) {
        (a.area.left, a.area.top) = G11_ORIGIN;
        // The rule adjusts a layout; a buffer without a row gets one with the usual band parameters.
        a.band.get_or_insert(geometry::BandParams { left_start: 2, left_guard: 2, right_gap: 2, right_trim: 0 });
    }
    // Per-model corrections on top of the table. For the G16 and S120 the sample data does not show
    // an edge where these margins put it; they are kept as they have always been applied.
    match model {
        "PowerShot G16" => {
            a.area =
                Geometry { left: 120, top: 29, width: raw_w.saturating_sub(168), height: raw_h.saturating_sub(43) };
            a.band = None;
            a.fixed_black = vec![Rect::new(0, 0, 16, 80)];
        }
        "PowerShot S120" if (raw_w, raw_h) == (4192, 3062) => {
            a.area = Geometry { left: 120, top: 31, width: 4022, height: 3016 };
            a.band = None;
            a.fixed_black = vec![Rect::new(23, 31, 49, 3016)];
        }
        "PowerShot SX50 HS" => a.area.top = 17,
        _ => {}
    }
    // No row: the maker note's own SensorInfo describes the image area when the buffer is its sensor.
    if !a.has_row && mn.color.is_some_and(|c| c.average_black() > 0) {
        if let Some(s) = mn.sensor.as_ref().filter(|s| (s.width as u32, s.height as u32) == (raw_w, raw_h)) {
            let c = &s.crop;
            let left = (c.left as u32 + 1) & !1;
            let top = (c.top as u32 + 1) & !1;
            a.area = Geometry {
                left,
                top,
                width: (c.right as u32 + 1).saturating_sub(left),
                height: (c.bottom as u32 + 1).saturating_sub(top),
            };
        }
    }
    a
}

fn open_bayer<'a>(
    data: &'a [u8],
    tiff: &Tiff<'a>,
    parsed: Parsed<'a>,
    opts: &OpenOptions,
) -> Result<Box<dyn RawDecoder + 'a>> {
    let Parsed { mut meta, mn, stream, orientation } = parsed;
    let (raw_w, raw_h) = frame_dims(&stream.hdr);
    opts.limits.check_dims(raw_w, raw_h, 1)?;
    let cam = db_lookup(&mut meta, "");
    let model = meta.model.trim();
    let model = model.strip_prefix("Canon ").unwrap_or(model).to_string();

    let a = bayer_area(raw_w, raw_h, &model, &mn);
    let Geometry { left, top, .. } = a.area;

    // Colour filter: the pattern at the active-area origin, moved to the buffer origin.
    let visible = stream
        .tiff_cfa
        .or_else(|| a.forced_cfa.and_then(CfaPattern::parse))
        .or_else(|| CfaPattern::parse(if stream.from_makernote { "GRBG" } else { "RGGB" }))
        .ok_or_else(|| Error::invalid_metadata("CR2: invalid CFA"))?;
    let (sx, sy) = (((2 - left % 2) % 2) as usize, ((2 - top % 2) % 2) as usize);

    // Even origin: an odd margin gives its first line up.
    let mut area = a.area;
    if area.top % 2 == 1 {
        area.top += 1;
        area.height = area.height.saturating_sub(1);
    }
    if area.left % 2 == 1 {
        area.left += 1;
        area.width = area.width.saturating_sub(1);
    }

    let mut info = FrameInfo::new(raw_w, raw_h, stream.hdr.bits as u8);
    info.compression = Compression::LosslessJpeg;
    info.orientation = orientation;
    info.layout = SensorLayout::Bayer;
    info.cfa = Some(visible.shifted(sx, sy));
    let active = Rect::new(area.left, area.top, area.width, area.height).clamp_to(raw_w, raw_h);
    info.active_area = active;
    info.crop = mn
        .image_area(raw_w, raw_h, false)
        .and_then(|r| r.intersect(&active))
        .map(|r| even_crop_origin(r, active))
        .unwrap_or(active);

    // Black level: measured from the optical-black rectangles when the geometry is known, else the
    // maker note's per-channel values, else the database's single value.
    let canonical = cam.map(|c| c.canonical_model);
    let fallback_black =
        flashraw_camera_db::find_matrix("Canon", canonical.unwrap_or(&model)).map_or(0, |m| m.black.max(0));
    info.black_level = BlackLevel::uniform(fallback_black as f32);
    // Without a table row the maker note's per-channel values are the measurement.
    let colordata = if a.has_row { None } else { mn.color.filter(|c| c.average_black() > 0).and_then(|c| c.black) };
    if let Some(black) = colordata {
        info.black_level = black_pattern(black, &visible, sx, sy);
    } else if a.band.is_some() || !a.fixed_black.is_empty() {
        let mut masked = a.fixed_black.clone();
        if let Some(band) = &a.band {
            let g = Geometry { left: active.x, top: active.y, width: active.width, height: active.height };
            for (x0, x1) in geometry::masked_columns(&g, raw_w, band) {
                if x1 > x0 && g.height > 0 {
                    masked.push(Rect::new(x0, g.top, x1 - x0, g.height));
                }
            }
        }
        info.black_from_masked = !masked.is_empty();
        info.masked_areas = masked;
    }
    let kodak = kodak_info(tiff);
    if let Some(b) = kodak.black {
        info.black_level = BlackLevel::uniform(b as f32);
    }

    // White level and balance.
    info.white_level = WhiteLevel::uniform(match stream.curve.as_deref() {
        Some(c) => c[c.len().min(0x1000) - 1] as u32,
        None => {
            let name = canonical.unwrap_or(&meta.canonical_model);
            match flashraw_camera_db::find_matrix("Canon", name).map(|m| m.white) {
                Some(w) if w > 0 => w as u32,
                _ => (1u32 << stream.hdr.bits.clamp(1, 16)) - 1,
            }
        }
    });
    let wb = mn.as_shot_wb().or(kodak.wb);
    info.white_balance = wb.and_then(|[r, g, b, g2]| WhiteBalance::from_multipliers(r, g, b, g2));
    info.color_matrices = db_color(&meta, cam);

    if stream.slices.is_sliced() {
        let s = stream.slices;
        meta.set_extra("cr2_slices", format!("{}x{}+{}", s.count, s.width, s.last_width));
    }

    let Stream { offset, bytes, declared, hdr, slices, curve, .. } = stream;
    let mapper = truncation_mapper(data.len(), offset, declared);
    let length = bytes.len() as u64;
    // The EOS-1D Mark III stream is offset by two samples.
    let shift = raw_w == 3984;
    let decode = move |ctx: &DecodeContext| -> Result<RawImage> {
        let mut img = RawImage::new_u16(raw_w, raw_h, 1, &ctx.limits)?;
        let out = img.u16_mut();
        ctx.profiler
            .time(Stage::EntropyDecode, || {
                canon_cr2::decode(bytes, &hdr, raw_w as usize, raw_h as usize, slices, shift, ctx.parallel, out)
            })
            .map_err(&mapper)?;
        if let Some(c) = &curve {
            let mut full = c.clone();
            full.resize(65536, c.last().copied().unwrap_or(0));
            ctx.profiler.time(Stage::Calibration, || apply_curve(out, &full, ctx.parallel));
        }
        Ok(img)
    };
    let mut dec = SimpleDecoder::new(RawFormat::Cr2, meta, info, Box::new(decode));
    dec.units = vec![DecodeUnit { rect: Rect::full(raw_w, raw_h), offset: offset as u64, length }];
    Ok(Box::new(dec))
}

// ---------------------------------------------------------------------------------------------
// sRAW / mRAW files
// ---------------------------------------------------------------------------------------------

/// Canon model ids whose sRAW conversion uses the full matrix: 5D Mark II, 7D, 50D, 1D Mark IV, 60D.
const SRAW_MATRIX_IDS: [u32; 5] = [0x8000_0218, 0x8000_0250, 0x8000_0261, 0x8000_0281, 0x8000_0287];
/// Ids below this one store a luma offset of 512 (the original conversion).
const SRAW_OFFSET_BELOW: u32 = 0x8000_0218;
/// Bodies from this id on (and the 5D Mark II from firmware 1.0.7) use the later hue rule.
const SRAW_NEWER_FROM: u32 = 0x8000_0281;
const SRAW_5D2: u32 = 0x8000_0218;

/// Output geometry of an sRAW frame.
struct SrawGeometry {
    out_w: u32,
    out_h: u32,
    raw_w: u32,
    active: Rect,
}

fn sraw_geometry(frame: (u32, u32), model: &str, mn: &CanonMakernote) -> SrawGeometry {
    let (mut raw_w, mut raw_h) = frame;
    let (mut left, mut top, mut width, mut height) = (0, 0, raw_w, raw_h);
    if let Some(row) = geometry::find(raw_w, raw_h) {
        let g = row.geometry();
        (left, top, width, height) = (g.left, g.top, g.width, g.height);
    }
    // Frames whose last columns are padding (EOS 50D and 5D Mark II sRAW1).
    width = match width {
        3344 => 3272,
        3872 => 3866,
        w => w,
    };
    if height > width {
        std::mem::swap(&mut width, &mut height);
        std::mem::swap(&mut raw_w, &mut raw_h);
    }
    if width == 7200 && height == 3888 {
        (raw_w, raw_h) = (6480, 4320);
        (width, height) = (6480, 4320);
    }
    let ratio = raw_h as f32 / raw_w as f32;
    let mut full_frame = false;
    match mn.sensor.as_ref().filter(|s| !(0.57..=0.75).contains(&ratio) && s.width > 1 && s.height > 1) {
        Some(s) => {
            (raw_w, raw_h) = (s.width as u32, s.height as u32);
            (left, top) = (s.crop.left as u32, s.crop.top as u32);
            (width, height) = (s.crop.width(), s.crop.height());
            full_frame = true;
        }
        None if raw_w == 4032 && raw_h == 3402 && model.eq_ignore_ascii_case("Canon EOS 80D") => {
            (raw_w, left, width, raw_h, top, height) = (4536, 28, 4508, 3024, 8, 3016);
            full_frame = true;
        }
        None => {}
    }
    let (out_w, out_h) = if full_frame { (raw_w, raw_h) } else { (width, height) };
    let active = if full_frame { Rect::new(left, top, width, height) } else { Rect::full(out_w, out_h) };
    SrawGeometry { out_w, out_h, raw_w, active }
}

fn open_sraw<'a>(data: &'a [u8], parsed: Parsed<'a>, opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    let Parsed { mut meta, mn, stream, orientation } = parsed;
    let sraw = stream.hdr.sraw;
    let subsampling = if sraw >= 3 { "4:2:0" } else { "4:2:2" };
    let cam = db_lookup(&mut meta, if sraw >= 3 { "medium-raw" } else { "small-raw" });

    let g = sraw_geometry(frame_dims(&stream.hdr), meta.model.trim(), &mn);
    opts.limits.check_dims(g.out_w, g.out_h, 3)?;

    let id = mn.model_id.unwrap_or(0);
    let conversion = if SRAW_MATRIX_IDS.contains(&id) {
        SrawConversion::Matrix
    } else {
        SrawConversion::Original { subtract_512: id < SRAW_OFFSET_BELOW }
    };
    let newer = id >= SRAW_NEWER_FROM || (id == SRAW_5D2 && mn.firmware_number().unwrap_or(0) > 1_000_006);
    let mul = mn.color.and_then(|c| c.sraw_mul).map_or([0; 3], |m| [m[0], m[1], m[2]]);
    let params = SrawParams {
        width: g.out_w as usize,
        height: g.out_h as usize,
        raw_width: g.raw_w as usize,
        slices: [stream.slices.count as u16, stream.slices.width as u16, stream.slices.last_width as u16],
        conversion,
        hue: SrawParams::default_hue(sraw, newer),
        mul,
    };

    let mut info = FrameInfo::new(g.out_w, g.out_h, 15);
    info.cpp = 3;
    info.layout = SensorLayout::Linear;
    info.cfa = None;
    info.compression = Compression::CanonSraw;
    info.orientation = orientation;
    info.white_level = WhiteLevel::uniform(0x3FFF);
    info.black_level = BlackLevel::uniform(0.0);
    info.active_area = g.active;
    info.crop = mn.image_area(g.out_w, g.out_h, true).and_then(|r| r.intersect(&g.active)).unwrap_or(g.active);
    info.white_balance = mn.as_shot_wb().and_then(|[r, gr, b, g2]| WhiteBalance::from_multipliers(r, gr, b, g2));
    info.color_matrices = db_color(&meta, cam);
    meta.set_extra("sraw_subsampling", subsampling);

    let Stream { offset, bytes, declared, hdr, .. } = stream;
    let mapper = truncation_mapper(data.len(), offset, declared);
    let length = bytes.len() as u64;
    let (out_w, out_h) = (g.out_w, g.out_h);
    let decode = move |ctx: &DecodeContext| -> Result<RawImage> {
        let mut img = RawImage::new_u16(out_w, out_h, 3, &ctx.limits)?;
        let out = img.u16_mut();
        ctx.profiler
            .time(Stage::EntropyDecode, || canon_sraw::decode(bytes, &hdr, &params, ctx.parallel, out))
            .map_err(&mapper)?;
        Ok(img)
    };
    let mut dec = SimpleDecoder::new(RawFormat::Cr2, meta, info, Box::new(decode));
    dec.units = vec![DecodeUnit { rect: Rect::full(out_w, out_h), offset: offset as u64, length }];
    Ok(Box::new(dec))
}
