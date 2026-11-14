//! Panasonic RW2 and Leica RWL files (and the early Panasonic/Leica TIFF-like RAW files).
//!
//! The container is a TIFF variant (magic `IIU\0`) whose first directory uses Panasonic's own tag
//! numbers: sensor size and borders, CFA code, bit depth, linearity limits, black levels, white
//! balance and the raw-format version that selects one of the packed or entropy-coded payloads
//! (formats 4 to 8). The early bodies (no tag 0x118) store a plain TIFF strip instead.

use crate::common::*;
use flashraw_codecs::packed::{self, PackedLayout};
use flashraw_codecs::panasonic as codec;
use flashraw_codecs::panasonic::{BlockedStream, V4_PIXELS, V8Decoder, V8Params, V8Strip};
use flashraw_core::tiff::{Entry, Ifd, Tiff, tags};
use flashraw_core::*;

/// The RW2 magic `IIU\0` (at least 8 bytes of data).
pub fn probe(data: &[u8]) -> bool {
    data.len() >= 8 && data.starts_with(b"IIU\0")
}

/// Parse and open a file whose magic [`probe`] accepted.
pub fn open<'a>(data: &'a [u8], opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    let tiff = Tiff::parse(data)?;
    open_tiff(data, tiff, opts)
}

// ---------------------------------------------------------------------------------------------
// Tags
// ---------------------------------------------------------------------------------------------

mod tag {
    pub const SENSOR_WIDTH: u16 = 0x02;
    pub const SENSOR_HEIGHT: u16 = 0x03;
    pub const BORDER_TOP: u16 = 0x04;
    pub const BORDER_LEFT: u16 = 0x05;
    pub const BORDER_BOTTOM: u16 = 0x06;
    pub const BORDER_RIGHT: u16 = 0x07;
    pub const CFA_CODE: u16 = 0x09;
    pub const BITS: u16 = 0x0A;
    /// Linearity limits of the red, green and blue channels.
    pub const LIMITS: [u16; 3] = [0x0E, 0x0F, 0x10];
    pub const BALANCE_RED: u16 = 0x11;
    pub const BALANCE_BLUE: u16 = 0x12;
    pub const ISO: u16 = 0x17;
    /// Black levels of red, green and blue.
    pub const BLACKS: [u16; 3] = [0x1C, 0x1D, 0x1E];
    /// White levels of red, green and blue.
    pub const WB_LEVELS: [u16; 3] = [0x24, 0x25, 0x26];
    pub const RAW_FORMAT: u16 = 0x2D;
    pub const JPG_FROM_RAW: u16 = 0x2E;
    pub const ISO_ALT: u16 = 0x37;
    pub const V8_SLOPES: u16 = 0x39;
    pub const V8_POINTS: u16 = 0x3A;
    pub const V8_CLIP: u16 = 0x3B;
    pub const V8_INITIAL: u16 = 0x3C;
    pub const V8_CODES: u16 = 0x40;
    pub const V8_SHIFTS: u16 = 0x41;
    pub const V8_STRIPS_ACROSS: u16 = 0x42;
    pub const V8_STRIPS_DOWN: u16 = 0x43;
    pub const V8_OFFSETS: u16 = 0x44;
    pub const V8_ORIGINS: u16 = 0x45;
    pub const V8_BITS: u16 = 0x46;
    pub const V8_WIDTHS: u16 = 0x47;
    pub const V8_HEIGHTS: u16 = 0x48;
    pub const PAYLOAD_OFFSET: u16 = 0x118;
}

/// The value of a tag, as the first element of its entry.
fn value(ifd: &Ifd, t: u16) -> Option<u32> {
    ifd.get(t).and_then(|e| e.u32(0))
}

/// A counted array stored in the entry's bytes: a little-endian `u16` count, then that many
/// little-endian values of `size` bytes (the declared type is ignored). Values that do not fit
/// in the entry are left out.
fn counted(e: &Entry, size: usize) -> Vec<u32> {
    let b = e.data;
    let n = b.get(..2).map_or(0, |c| usize::from(u16::from_le_bytes([c[0], c[1]])));
    b.get(2..)
        .unwrap_or(&[])
        .chunks_exact(size)
        .take(n)
        .map(|c| c.iter().rev().fold(0u32, |acc, &x| (acc << 8) | u32::from(x)))
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Raw formats
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Format {
    V4,
    V5,
    V6,
    V7,
    V8,
}

/// The raw-format versions of files with a payload tag. Corpus: most files are version 4; 5 on
/// GH5S and some G9; 6 on S1/S1H/S1R/S5/GH5M2; 7 on three S5 files; 8 on GH6, GH7, G9M2, S1M2,
/// S1RM2, S5M2, S9 and later bodies.
const FORMATS: [(u32, Format, Compression, &str); 5] = [
    (4, Format::V4, Compression::Panasonic, "v4"),
    (5, Format::V5, Compression::PanasonicV5, "v5"),
    (6, Format::V6, Compression::PanasonicV6, "v6"),
    (7, Format::V7, Compression::PanasonicV7, "v7"),
    (8, Format::V8, Compression::PanasonicV8, "v8"),
];

/// Where the payload of a file without tag 0x118 starts (early bodies).
const BLOCK_SPLIT: usize = 0x2008;

enum Payload<'a> {
    V4(BlockedStream<'a>),
    V5(BlockedStream<'a>),
    V6(&'a [u8]),
    V7(&'a [u8]),
    V8 {
        decoder: Box<V8Decoder>,
        strips: Vec<V8Strip<'a>>,
        /// Declared `(offset, length)` of each strip.
        declared: Vec<(usize, usize)>,
    },
    /// 12-bit rows, tight or with a pad byte after every 15 data bytes.
    Packed12(&'a [u8]),
    /// 16-bit little-endian words with 12 bits left-aligned.
    Unpacked,
}

/// A payload chosen together with its metadata names.
struct Chosen<'a> {
    payload: Payload<'a>,
    compression: Compression,
    mode_name: &'static str,
    /// The `panasonic_raw_format` value.
    version: u32,
    offset: usize,
}

/// The format-8 parameters and strips.
fn read_v8<'a>(data: &'a [u8], ifd: &Ifd, (raw_w, raw_h): (u32, u32)) -> Result<Payload<'a>> {
    let need = |t: u16| ifd.get(t).ok_or_else(|| Error::invalid_metadata(format!("missing RW2 v8 tag {t:#x}")));
    let slopes = counted(need(tag::V8_SLOPES)?, 4);
    let points = counted(need(tag::V8_POINTS)?, 4);
    let mut params = V8Params { clip: value(ifd, tag::V8_CLIP).unwrap_or(0xFFFF), ..V8Params::default() };
    for i in 0..6 {
        params.gamma_slopes[i] = slopes.get(i).copied().unwrap_or(0);
        params.gamma_points[i] = points.get(i).map_or(0, |p| p >> 16);
    }
    for (i, slot) in params.initial.iter_mut().enumerate() {
        *slot = value(ifd, tag::V8_INITIAL + i as u16).unwrap_or(0) as u16;
    }
    params.codes = counted(need(tag::V8_CODES)?, 4)
        .into_iter()
        .map(|v| ((v & 0xFFFF).min(16) as u8, ((v >> 16) & 0xFFF) as u16))
        .collect();
    params.shift_down = counted(need(tag::V8_SHIFTS)?, 2).into_iter().map(|v| v.min(64) as u8).collect();

    let across = u64::from(value(ifd, tag::V8_STRIPS_ACROSS).unwrap_or(0));
    let down = u64::from(value(ifd, tag::V8_STRIPS_DOWN).unwrap_or(1).max(1));
    let count = across * down;
    if count == 0 || count > 64 {
        return Err(Error::corrupt(format!("invalid RW2 v8 strip count {count}")));
    }
    let offsets = counted(need(tag::V8_OFFSETS)?, 4);
    let origins = counted(need(tag::V8_ORIGINS)?, 4);
    let bit_counts = counted(need(tag::V8_BITS)?, 4);
    let widths = counted(need(tag::V8_WIDTHS)?, 2);
    let heights = counted(need(tag::V8_HEIGHTS)?, 2);

    let (mut strips, mut declared, mut covered) = (Vec::new(), Vec::new(), 0u64);
    for i in 0..count as usize {
        let (Some(&off), Some(&origin), Some(&bits), Some(&width), Some(&height)) =
            (offsets.get(i), origins.get(i), bit_counts.get(i), widths.get(i), heights.get(i))
        else {
            return Err(Error::corrupt("RW2 v8 strip table too short"));
        };
        let (off, len) = (off as usize, (bits as usize).div_ceil(8));
        let (left, top) = (origin & 0xFFFF, origin >> 16);
        if u64::from(left) + u64::from(width) > u64::from(raw_w)
            || u64::from(top) + u64::from(height) > u64::from(raw_h)
        {
            return Err(Error::corrupt("RW2 v8 strip outside the image"));
        }
        let start = off.min(data.len());
        let end = off.saturating_add(len).min(data.len());
        strips.push(V8Strip { data: &data[start..end], bits: u64::from(bits), left, top, width, height });
        declared.push((off, len));
        covered += u64::from(width) * u64::from(height);
    }
    if covered != u64::from(raw_w) * u64::from(raw_h) {
        return Err(Error::corrupt("RW2 v8 strips do not cover the image"));
    }
    Ok(Payload::V8 { decoder: Box::new(V8Decoder::new(&params)?), strips, declared })
}

fn choose_payload<'a>(data: &'a [u8], ifd: &Ifd, (raw_w, raw_h): (u32, u32)) -> Result<Chosen<'a>> {
    let version = value(ifd, tag::RAW_FORMAT);
    if ifd.has(tag::PAYLOAD_OFFSET) {
        // Formats 4 to 8.
        let offset = value(ifd, tag::PAYLOAD_OFFSET).unwrap_or(0) as usize;
        let body = data.get(offset..).unwrap_or(&[]);
        let v = version.unwrap_or(4);
        let Some(&(_, format, compression, mode_name)) = FORMATS.iter().find(|f| f.0 == v) else {
            return Err(Error::unsupported_compression("RW2", format!("raw format {v}")));
        };
        let payload = match format {
            Format::V4 => Payload::V4(BlockedStream::new(body, BLOCK_SPLIT)?),
            Format::V5 => Payload::V5(BlockedStream::new(body, BLOCK_SPLIT)?),
            Format::V6 => Payload::V6(body),
            Format::V7 => Payload::V7(body),
            Format::V8 => read_v8(data, ifd, (raw_w, raw_h))?,
        };
        return Ok(Chosen { payload, compression, mode_name, version: v, offset });
    }

    // Early bodies: a plain strip.
    let offset =
        value(ifd, tags::STRIP_OFFSETS).ok_or_else(|| Error::invalid_metadata("RW2 without payload offset"))? as usize;
    let body = data.get(offset..).unwrap_or(&[]);
    let (w, h) = (raw_w as usize, raw_h as usize);
    let blocked_rows = w >= 7 && body.len() / (w * 8 / 7) == h;
    let twelve_bit = value(ifd, tag::BITS).unwrap_or(0) == 12 || ifd.bps() == 12;
    let (payload, compression, mode_name) = if blocked_rows {
        (Payload::V4(BlockedStream::new(body, 0)?), Compression::Panasonic, "v4")
    } else if twelve_bit && body.len() < w.saturating_mul(h).saturating_mul(2) {
        (Payload::Packed12(body), Compression::Uncompressed, "packed12")
    } else {
        (Payload::Unpacked, Compression::Uncompressed, "unpacked")
    };
    let version = version.unwrap_or(if matches!(payload, Payload::V4(_)) { 4 } else { 0 });
    Ok(Chosen { payload, compression, mode_name, version, offset })
}

// ---------------------------------------------------------------------------------------------
// Geometry, colour filter, levels
// ---------------------------------------------------------------------------------------------

/// CFA codes of tag 0x09, indexed by code - 1.
const CFA_CODES: [CfaPattern; 4] = [CfaPattern::RGGB, CfaPattern::GRBG, CfaPattern::GBRG, CfaPattern::BGGR];

/// Files whose CFA code is known to be wrong for one sensor size. UNVERIFIED: no corpus file has
/// this size (the corpus LX100 II is 3696x3568 and states code 4).
/// `(base model, raw width, raw height, stated code, true code)`.
const CFA_CODE_FIXES: [(&str, u32, u32, u32, u32); 1] = [("DC-LX100M2", 4816, 3568, 3, 4)];

/// ISO tags used when the EXIF data has no usable ISO. UNVERIFIED: every corpus file has an EXIF ISO.
const ISO_TAGS: [u16; 2] = [tag::ISO, tag::ISO_ALT];
/// Marker of "no ISO" in the first ISO tag.
const ISO_NONE: u32 = 65535;

/// Linearity limits are stated as the clip point; the data saturate a little below it.
/// `(stated, used)`; the mapping is the stated point minus 64 and minus 16. Conventions reproduced by
/// every corpus file but not visible in image statistics.
const LIMIT_MAP: [(u32, u32); 2] = [(16383, 16319), (4095, 4079)];

/// Added to the stated black levels of format-4 bodies (confirmed by the optical-black columns of
/// one file, reproduced by about 100 more).
const V4_BLACK_OFFSET: u32 = 15;

/// Raw formats from here on state exact black levels and linearity limits.
const MODERN_FROM: u32 = 6;

/// Aspect ratios the early file layouts are named by: the ratio, the camera-database mode
/// of that layout and the ratio's value.
const ASPECTS: [(&str, &str, f32); 4] = [
    ("16:9", "aspect-16x9", 16.0 / 9.0),
    ("3:2", "aspect-3x2", 1.5),
    ("4:3", "aspect-4x3", 4.0 / 3.0),
    ("1:1", "aspect-1x1", 1.0),
];

/// The nearest aspect ratio of a `w x h` buffer (on equal distance the earlier entry), as
/// `(ratio, database mode)`.
fn guess_aspect(w: u32, h: u32) -> (&'static str, &'static str) {
    let ratio = w as f32 / h.max(1) as f32;
    let mut best = (&ASPECTS[0], (ratio - ASPECTS[0].2).abs());
    for a in &ASPECTS[1..] {
        let d = (ratio - a.2).abs();
        if d < best.1 {
            best = (a, d);
        }
    }
    (best.0.0, best.0.1)
}

/// Rows added to the height the bottom border states (a convention reproduced by every corpus body).
const HEIGHT_PAD: i64 = 12;

/// Active area from the border tags, the database size rule and the even-origin rule.
fn active_area((raw_w, raw_h): (u32, u32), borders: [Option<u32>; 4]) -> Rect {
    let [_, left_b, bottom_b, right_b] = borders;
    let mut width = match (left_b, right_b) {
        (Some(l), Some(r)) => i64::from(l) + i64::from(r),
        (Some(l), None) => i64::from(l),
        _ => 0,
    };
    let mut height = i64::from(bottom_b.unwrap_or(0));
    if width == 0 {
        width = i64::from(raw_w);
    }
    if height == 0 {
        height = i64::from(raw_h);
    }
    height = (height + HEIGHT_PAD).min(i64::from(raw_h));
    let (mut left, mut top) = (0i64, 0i64);
    if let Some(m) = flashraw_camera_db::size_rule("Panasonic", raw_w, raw_h) {
        left = i64::from(m.left);
        top = i64::from(m.top);
        width += i64::from(m.width_delta);
        height += i64::from(m.height_delta);
    }
    if top % 2 != 0 {
        top += 1;
        height -= 1;
    }
    if left % 2 != 0 {
        left += 1;
        width -= 1;
    }
    let c = |v: i64| v.clamp(0, i64::from(u32::MAX)) as u32;
    Rect::new(c(left), c(top), c(width), c(height)).clamp_to(raw_w, raw_h)
}

/// The default crop: the border rectangle inside the active area.
fn border_crop(borders: [Option<u32>; 4], active: Rect) -> Rect {
    if let [Some(top), Some(left), Some(bottom), Some(right)] = borders {
        if right > left && bottom > top {
            if let Some(r) = Rect::new(left, top, right - left, bottom - top).intersect(&active) {
                return r;
            }
        }
    }
    active
}

fn cfa_pattern(ifd: &Ifd, base_model: &str, (raw_w, raw_h): (u32, u32)) -> CfaPattern {
    let mut code = value(ifd, tag::CFA_CODE).unwrap_or(1);
    for &(model, w, h, stated, truth) in &CFA_CODE_FIXES {
        if base_model.eq_ignore_ascii_case(model) && (raw_w, raw_h) == (w, h) && code == stated {
            code = truth;
        }
    }
    code.checked_sub(1).and_then(|i| CFA_CODES.get(i as usize)).copied().unwrap_or(CfaPattern::RGGB)
}

/// A 2x2 black pattern with the red, green and blue levels at their sites.
fn black_pattern(cfa: &CfaPattern, [r, g, b]: [u32; 3]) -> BlackLevel {
    let level = |i: usize| match cfa.color_at(i % 2, i / 2) {
        CfaColor::Red => r,
        CfaColor::Blue => b,
        _ => g,
    };
    BlackLevel::pattern(2, 2, (0..4).map(|i| level(i) as f32).collect())
}

// ---------------------------------------------------------------------------------------------
// Open
// ---------------------------------------------------------------------------------------------

/// The directory with the raw image: it has the payload tag, or the sensor size and a strip.
fn raw_ifd<'t, 'a>(tiff: &'t Tiff<'a>) -> Option<&'t Ifd<'a>> {
    tiff.ifds.iter().find(|i| {
        i.has(tag::PAYLOAD_OFFSET)
            || (i.has(tag::SENSOR_WIDTH) && i.has(tag::SENSOR_HEIGHT) && i.has(tags::STRIP_OFFSETS))
    })
}

/// Open a Panasonic/Leica file given as an already parsed TIFF.
pub fn open_tiff<'a>(data: &'a [u8], tiff: Tiff<'a>, opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    let ifd = raw_ifd(&tiff)
        .ok_or_else(|| no_raw_ifd(&tiff, "Panasonic", Error::invalid_metadata("Panasonic file without raw IFD")))?;
    let mut meta = RawMetadata::default();
    flashraw_core::exif::read_ifd0(ifd, &mut meta);
    for other in tiff.ifds.iter().skip(1) {
        flashraw_core::exif::read_ifd0(other, &mut meta);
    }
    let raw_w = value(ifd, tag::SENSOR_WIDTH).unwrap_or(0);
    let raw_h = value(ifd, tag::SENSOR_HEIGHT).unwrap_or(0);
    opts.limits.check_dims(raw_w, raw_h, 1)?;
    if raw_w < 22 || raw_h < 22 {
        return Err(Error::invalid_metadata(format!("invalid RW2 raw size {raw_w}x{raw_h}")));
    }
    let bps = value(ifd, tag::BITS).unwrap_or(12).clamp(8, 16);
    if meta.exposure.iso.is_none_or(|i| i == ISO_NONE) {
        let first = value(ifd, ISO_TAGS[0]).filter(|&v| v != 0 && v != ISO_NONE);
        meta.exposure.iso = first.or_else(|| value(ifd, ISO_TAGS[1]));
    }

    let chosen = choose_payload(data, ifd, (raw_w, raw_h))?;
    meta.set_extra("panasonic_raw_format", chosen.version);
    meta.set_extra("encoding", chosen.mode_name);

    // Geometry.
    let borders = [tag::BORDER_TOP, tag::BORDER_LEFT, tag::BORDER_BOTTOM, tag::BORDER_RIGHT].map(|t| value(ifd, t));
    let active = active_area((raw_w, raw_h), borders);
    let crop = border_crop(borders, active);
    let orientation = Orientation::from_exif(ifd.u32(tags::ORIENTATION).unwrap_or(1));

    // Colour filter, names, table.
    let base_model = flashraw_camera_db::base_model("Panasonic", &meta.model);
    let cfa = cfa_pattern(ifd, base_model, (raw_w, raw_h));
    // The colour table row for the body: the longest model prefix that ends at a word
    // boundary ("DC-G9" is not a row for "DC-G97" or "DC-G9M2").
    let fallback = flashraw_camera_db::find_matrix("Panasonic", base_model);
    let (aspect, mode) = guess_aspect(raw_w, raw_h);
    let cam = db_lookup(&mut meta, mode);
    meta.set_extra("aspect", aspect);

    // Levels.
    let modern = chosen.version >= MODERN_FROM;
    let stated_black = tag::BLACKS.map(|t| value(ifd, t));
    let mut known = Known { cfa: true, crop: true, active_area: true, white: true, ..Known::default() };
    let black = if modern {
        Some(black_pattern(&cfa, stated_black.map(|v| v.unwrap_or(0))))
    } else if let [Some(r), Some(g), Some(b)] = stated_black.map(|v| v.filter(|&v| v > 0)) {
        let add = if chosen.version == 4 { V4_BLACK_OFFSET } else { 0 };
        Some(black_pattern(&cfa, [r, g, b].map(|v| v.saturating_add(add))))
    } else {
        fallback.filter(|t| t.black != 0).map(|t| BlackLevel::uniform(t.black.unsigned_abs() as f32))
    };
    known.black = black.is_some();
    let limits: Vec<u32> = tag::LIMITS
        .iter()
        .filter_map(|&t| value(ifd, t))
        .map(|v| LIMIT_MAP.iter().find(|m| m.0 == v).map_or(v, |m| m.1))
        .collect();
    let bits_max = if bps >= 16 { 65535 } else { (1u32 << bps) - 1 };
    let white = match (modern.then(|| limits.iter().max()).flatten(), fallback) {
        (Some(&w), _) => w,
        (None, Some(t)) if t.white > 0 => t.white as u32,
        _ => bits_max,
    };

    let mut info = FrameInfo::new(raw_w, raw_h, bps.min(16) as u8);
    info.cfa = Some(cfa);
    info.layout = SensorLayout::Bayer;
    info.active_area = active;
    info.crop = crop;
    info.orientation = orientation;
    info.compression = chosen.compression;
    info.white_level = WhiteLevel::uniform(white);
    if let Some(b) = black {
        info.black_level = b;
    }
    let wb_levels = tag::WB_LEVELS.map(|t| value(ifd, t));
    info.white_balance = match (wb_levels, value(ifd, tag::BALANCE_RED), value(ifd, tag::BALANCE_BLUE)) {
        ([Some(r), Some(g), Some(b)], _, _) => WhiteBalance::from_multipliers(r as f32, g as f32, b as f32, g as f32),
        (_, Some(r), Some(b)) => WhiteBalance::from_multipliers(r as f32, 256.0, b as f32, 256.0),
        _ => None,
    };
    // The body's own database entry carries its matrix; the table row is the fallback.
    if let Some(t) = fallback.filter(|_| cam.is_none_or(|c| c.color_matrix.is_empty())) {
        let m: Vec<f32> = t.color_matrix.iter().take(9).map(|&v| v as f32 / 10000.0).collect();
        if let Some(cm) = ColorMatrix::new(Illuminant::D65, 3, m) {
            info.color_matrices =
                ColorMatrices { color: vec![cm], source: ColorSource::CameraDb, ..Default::default() };
            known.color = true;
        }
    }
    apply_db(&mut info, &meta, cam, known);

    meta.previews = tiff_previews(data, 0, &tiff, orientation);
    if let Some(e) = ifd.get(tag::JPG_FROM_RAW) {
        push_jpeg(data, e.value_pos, e.bytes().len(), orientation, &mut meta.previews);
    }
    classify_previews(&mut meta.previews);

    // The decoder object.
    let (random_access, units) = match &chosen.payload {
        Payload::V8 { strips, declared, .. } => (
            RandomAccess::Blocks,
            strips
                .iter()
                .zip(declared)
                .map(|(s, &(o, l))| DecodeUnit {
                    rect: Rect::new(s.left, s.top, s.width, s.height),
                    offset: o as u64,
                    length: l as u64,
                })
                .collect(),
        ),
        Payload::V4(_) if raw_w as usize % V4_PIXELS != 0 => (RandomAccess::Sequential, Vec::new()),
        _ => (RandomAccess::Rows, Vec::new()),
    };
    let decoder = std::sync::Arc::new(Decoder {
        data,
        payload_offset: chosen.offset,
        w: raw_w as usize,
        h: raw_h as usize,
        bits: bps,
        payload: chosen.payload,
    });
    let full = std::sync::Arc::clone(&decoder);
    let mut dec = SimpleDecoder::new(
        RawFormat::Rw2,
        meta,
        info,
        Box::new(move |ctx: &DecodeContext| {
            let mut img = RawImage::new_u16(raw_w, raw_h, 1, &ctx.limits)?;
            ctx.profiler.time(Stage::EntropyDecode, || full.decode(ctx, img.u16_mut()))?;
            Ok(img)
        }),
    );
    dec.region = Some(Box::new(move |ctx, rect| {
        let r = rect
            .intersect(&Rect::full(raw_w, raw_h))
            .ok_or_else(|| Error::InvalidArgument(format!("region {rect} outside image")))?;
        ctx.profiler.time(Stage::EntropyDecode, || decoder.decode_region(ctx, r))
    }));
    dec.random_access = random_access;
    dec.units = units;
    Ok(Box::new(dec))
}

// ---------------------------------------------------------------------------------------------
// Decoding
// ---------------------------------------------------------------------------------------------

struct Decoder<'a> {
    data: &'a [u8],
    payload_offset: usize,
    w: usize,
    h: usize,
    /// Bits per sample, 8 to 16.
    bits: u32,
    payload: Payload<'a>,
}

impl Decoder<'_> {
    /// Bytes the whole image needs, for the truncation accounting of the coded payloads.
    fn bytes_needed(&self) -> usize {
        let (w, h) = (self.w, self.h);
        let per_packet = codec::packed_pixels(self.bits).unwrap_or(1);
        let banded = h / 16 * 16;
        match &self.payload {
            Payload::V4(_) => (w.saturating_mul(h)).div_ceil(V4_PIXELS).saturating_mul(16),
            Payload::V5(..) => w.div_ceil(per_packet).saturating_mul(h).saturating_mul(16),
            Payload::V6(..) => (w / if self.bits == 12 { 14 } else { 11 }).saturating_mul(16).saturating_mul(banded),
            Payload::V7(..) => (w / per_packet).saturating_mul(16).saturating_mul(banded),
            _ => 0,
        }
    }

    fn truncated(&self, e: Error) -> Error {
        e.or_truncated(self.data.len(), DataUnit::Payload, self.payload_offset, self.bytes_needed())
    }

    /// Whole image into `out`.
    fn decode(&self, ctx: &DecodeContext, out: &mut [u16]) -> Result<()> {
        let (w, h, par, bits) = (self.w, self.h, ctx.parallel, self.bits);
        let map = |r: Result<()>| r.map_err(|e| self.truncated(e));
        match &self.payload {
            Payload::V4(s) => map(codec::decode_v4(s, w, h, out, par)),
            Payload::V5(s) => map(codec::decode_v5(s, bits, w, h, out, par)),
            Payload::V6(body) => map(codec::decode_v6(body, bits, w, h, out, par)),
            Payload::V7(body) => map(codec::decode_v7(body, bits, w, h, out, par)),
            Payload::V8 { decoder, strips, declared } => {
                for (i, &(o, l)) in declared.iter().enumerate() {
                    flashraw_core::integrity::check_range(self.data.len(), DataUnit::Strip(i as u32), o, l)
                        .map_err(Error::truncated)?;
                }
                decoder.decode(strips, out, w, h, par)
            }
            Payload::Packed12(body) => self.unpack_packed12(ctx, body, out, 0, h),
            Payload::Unpacked => self.unpack_words(ctx, out, 0, h),
        }
    }

    /// Whether rows can be decoded without the rest of the image.
    fn row_addressable(&self) -> bool {
        match &self.payload {
            Payload::V4(_) => self.w % V4_PIXELS == 0,
            Payload::V8 { .. } => false,
            _ => true,
        }
    }

    /// Rows `y0 .. y0 + rows` into `out` (`rows * w` samples).
    fn decode_rows(&self, ctx: &DecodeContext, out: &mut [u16], y0: usize, rows: usize) -> Result<()> {
        let (w, h, par, bits) = (self.w, self.h, ctx.parallel, self.bits);
        let map = |r: Result<()>| r.map_err(|e| self.truncated(e));
        match &self.payload {
            Payload::V4(s) => map(codec::decode_v4_rows(s, w, h, y0, out, par)),
            Payload::V5(s) => map(codec::decode_v5_rows(s, bits, w, h, y0, out, par)),
            Payload::V6(body) => map(codec::decode_v6_rows(body, bits, w, h, y0, out, par)),
            Payload::V7(body) => map(codec::decode_v7_rows(body, bits, w, h, y0, out, par)),
            Payload::Packed12(body) => self.unpack_packed12(ctx, body, out, y0, rows),
            Payload::Unpacked => self.unpack_words(ctx, out, y0, rows),
            Payload::V8 { .. } => unreachable!("format 8 is decoded whole"),
        }
    }

    fn decode_region(&self, ctx: &DecodeContext, r: Rect) -> Result<RawImage> {
        if self.row_addressable() {
            let mut band = RawImage::new_u16(self.w as u32, r.height, 1, &ctx.limits)?;
            self.decode_rows(ctx, band.u16_mut(), r.y as usize, r.height as usize)?;
            return band.crop(Rect::new(r.x, 0, r.width, r.height));
        }
        let mut img = RawImage::new_u16(self.w as u32, self.h as u32, 1, &ctx.limits)?;
        self.decode(ctx, img.u16_mut())?;
        img.crop(r)
    }

    /// 16-bit little-endian words, 12 bits left-aligned.
    fn unpack_words(&self, ctx: &DecodeContext, out: &mut [u16], y0: usize, rows: usize) -> Result<()> {
        let w = self.w;
        let geometry = RowGeometry::tight(y0 + rows, w, 16);
        let src = ctx.sensor_bytes(self.data, DataUnit::Strip(0), self.payload_offset, geometry.needed())?;
        let d = src.bytes.get(y0 * w * 2..).unwrap_or(&[]);
        let n = (d.len() / 2).min(w * rows);
        for (o, c) in out.iter_mut().zip(d.chunks_exact(2)).take(n) {
            *o = u16::from_le_bytes([c[0], c[1]]) >> 4;
        }
        if let Some(t) = src.truncation {
            ctx.recovered(CorruptReason::Truncated(t), (w * rows - n) as u64);
        }
        Ok(())
    }

    /// 12-bit rows, tight or padded (a pad byte after every 15 data bytes).
    fn unpack_packed12(&self, ctx: &DecodeContext, d: &[u8], out: &mut [u16], y0: usize, rows: usize) -> Result<()> {
        const GROUP: usize = 16;
        const GROUP_DATA: usize = 15;
        let w = self.w;
        let tight = w * 3 / 2;
        let padded = tight * GROUP / GROUP_DATA;
        if d.len() < padded.saturating_mul(self.h) {
            let geometry = RowGeometry::tight(y0 + rows, w, 12);
            let src = ctx.sensor_bytes(self.data, DataUnit::Strip(0), self.payload_offset, geometry.needed())?;
            let rest = src.bytes.get(y0 * tight..).unwrap_or(&[]);
            let layout = PackedLayout::tight(w, rows, 12, PackOrder::LsbBytes);
            packed::unpack(rest, layout, out, w, ctx.parallel, ctx.simd, ctx.recover)?;
            if let Some(t) = src.truncation {
                ctx.recovered(CorruptReason::Truncated(t), RowGeometry::tight(rows, w, 12).missing_samples(rest.len()));
            }
            return Ok(());
        }
        let simd = ctx.simd;
        par::for_each_row_chunk_bounded(out, w, par::memory_bound_workers(), ctx.parallel, |first, chunk| {
            let mut row = Vec::with_capacity(tight + GROUP);
            for (i, dst) in chunk.chunks_mut(w).enumerate() {
                let y = y0 + first + i;
                let src = d.get(y * padded..((y + 1) * padded).min(d.len())).unwrap_or(&[]);
                row.clear();
                for group in src.chunks(GROUP) {
                    row.extend_from_slice(&group[..group.len().min(GROUP_DATA)]);
                }
                packed::unpack_row(&row[..tight.min(row.len())], dst, 12, PackOrder::LsbBytes, simd);
            }
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{self, Tag, TiffWriter, ascii, long, short};

    #[test]
    fn probe_magic() {
        assert!(probe(b"IIU\0\x18\0\0\0"));
        assert!(!probe(b"IIU\0"));
        assert!(!probe(b"II*\0\x08\0\0\0"));
    }

    #[test]
    fn aspect_guess() {
        assert_eq!(guess_aspect(4000, 3000), ("4:3", "aspect-4x3"));
        assert_eq!(guess_aspect(6000, 4000), ("3:2", "aspect-3x2"));
        assert_eq!(guess_aspect(1920, 1080), ("16:9", "aspect-16x9"));
        assert_eq!(guess_aspect(3000, 3000), ("1:1", "aspect-1x1"));
    }

    #[test]
    fn model_aliases() {
        assert_eq!(flashraw_camera_db::base_model("Panasonic", "D-LUX 4"), "DMC-LX3");
        assert_eq!(flashraw_camera_db::base_model("Panasonic", "DC-GH5"), "DC-GH5");
        assert_eq!(flashraw_camera_db::base_model("Panasonic", "DC-TX2"), "DC-ZS200");
        assert_eq!(flashraw_camera_db::base_model("Panasonic", "DC-ZS220D"), "DC-ZS200");
    }

    #[test]
    fn table_rows_match_at_a_model_boundary() {
        let row = |m| flashraw_camera_db::find_matrix("Panasonic", m).map(|r| r.model_prefix);
        assert_eq!(row("DC-G9"), Some("DC-G9"));
        assert_eq!(row("DC-G9M2"), Some("DC-G9M2"));
        assert_eq!(row("DC-S1R"), Some("DC-S1R"));
        // A second-generation body is not its predecessor.
        assert_eq!(row("DC-S1M2"), None);
        assert_eq!(row("DC-S1RM2"), None);
    }

    #[test]
    fn counted_arrays() {
        let e = |typ, data: &'static [u8]| Entry {
            tag: 0,
            typ,
            count: data.len() as u32,
            order: ByteOrder::Little,
            data,
            value_field: 0,
            value_pos: 0,
        };
        assert_eq!(counted(&e(7, &[2, 0, 1, 0, 0, 0, 2, 0, 0, 0]), 4), vec![1, 2]);
        // A count beyond the bytes yields what fits; a short entry yields nothing.
        assert_eq!(counted(&e(7, &[9, 0, 1, 0]), 2), vec![1]);
        assert!(counted(&e(7, &[1]), 4).is_empty());
        assert_eq!(counted(&e(7, &[1, 0, 0x34, 0x12]), 2), vec![0x1234]);
    }

    #[test]
    fn cfa_codes_and_the_lx100_ii_fix() {
        let mut t = TiffWriter::new();
        let ifd = t.ifd(vec![short(0x09, &[3])]);
        let file = t.finish(ifd);
        let tiff = Tiff::parse(&file).unwrap();
        let ifd0 = &tiff.ifds[0];
        assert_eq!(cfa_pattern(ifd0, "DC-GH5", (4000, 3000)), CfaPattern::GBRG);
        assert_eq!(cfa_pattern(ifd0, "DC-LX100M2", (4816, 3568)), CfaPattern::BGGR);
        assert_eq!(cfa_pattern(ifd0, "dc-lx100m2", (3696, 3568)), CfaPattern::GBRG);
        let mut t = TiffWriter::new();
        let ifd = t.ifd(vec![short(0x09, &[9])]);
        let file = t.finish(ifd);
        let tiff = Tiff::parse(&file).unwrap();
        assert_eq!(cfa_pattern(&tiff.ifds[0], "X", (1, 1)), CfaPattern::RGGB);
    }

    /// Pack 9 samples of 14 bits into each 16-byte packet.
    fn pack14(samples: &[u16]) -> Vec<u8> {
        samples
            .chunks(9)
            .flat_map(|g| {
                let bits = g.iter().enumerate().fold(0u128, |a, (i, &s)| a | u128::from(s) << (14 * i));
                bits.to_le_bytes()
            })
            .collect()
    }

    fn v7_file(w: u32, h: u32, payload: &[u8]) -> Vec<u8> {
        let mut t = TiffWriter::new();
        t.buf.extend(payload);
        let tags: Vec<Tag> = vec![
            short(0x02, &[w as u16]),
            short(0x03, &[h as u16]),
            short(0x04, &[2]),
            short(0x05, &[4]),
            short(0x06, &[(h - 2) as u16]),
            short(0x07, &[(w - 4) as u16]),
            short(0x09, &[2]),
            short(0x0A, &[14]),
            short(0x0E, &[16383]),
            short(0x0F, &[16383]),
            short(0x10, &[16383]),
            short(0x1C, &[510]),
            short(0x1D, &[512]),
            short(0x1E, &[514]),
            short(0x24, &[500]),
            short(0x25, &[256]),
            short(0x26, &[400]),
            short(0x2D, &[7]),
            short(0x112, &[6]),
            long(0x118, &[8]),
            ascii(0x010F, "Panasonic"),
            ascii(0x0110, "DC-S5"),
        ];
        let ifd = t.ifd(tags);
        t.finish(ifd)
    }

    #[test]
    fn synthetic_format_7_file() {
        let (w, h) = (72u32, 32u32);
        let samples: Vec<u16> = (0..w * h).map(|i| ((i * 97) % 16384) as u16).collect();
        let file = v7_file(w, h, &pack14(&samples));
        let dec = crate::open(&file, &OpenOptions::default()).unwrap();
        let info = dec.frame_info();
        assert_eq!(info.cfa, Some(CfaPattern::GRBG));
        assert_eq!(info.white_level.values[0], 16319);
        assert_eq!(info.crop, Rect::new(4, 2, 64, 28));
        assert_eq!(info.active_area, Rect::new(0, 0, 72, 32));
        assert_eq!(info.black_level.values, vec![512.0, 510.0, 514.0, 512.0]);
        assert_eq!(info.white_balance.unwrap().coeffs[0], 500.0 / 256.0);
        assert_eq!(dec.metadata().extra.iter().find(|(k, _)| k == "panasonic_raw_format").unwrap().1, "7");
        assert_eq!(dec.random_access(), RandomAccess::Rows);
        let img = crate::nikon::conventions::robustness::open_decode(&file).unwrap();
        assert_eq!(img.u16(), &samples[..]);
        assert!(crate::nikon::conventions::robustness::region_matches_full(&file));
    }

    #[test]
    fn truncated_format_7_is_structured_truncation() {
        let (w, h) = (72u32, 32u32);
        let samples = vec![1u16; (w * h) as usize];
        let payload = pack14(&samples);
        // Declare the whole payload but keep the file cut in the middle of the packets.
        let file = v7_file(w, h, &payload[..payload.len() / 2]);
        let dec = crate::open(&file, &OpenOptions::default()).unwrap();
        let prof = Profiler::new();
        let ctx = DecodeContext::new(&prof);
        let err = dec.decode(&ctx).err().unwrap();
        assert!(err.truncation().is_some(), "{err}");
    }

    #[test]
    fn mutated_real_files_do_not_panic() {
        for rel in ["Panasonic/DMC-FZ8/P1240856.RAW", "Panasonic/DC-S1R/P1000077.RW2", "Panasonic/DC-GH7/P1002298.RW2"]
        {
            if let Some(d) = testkit::corpus(rel) {
                crate::nikon::conventions::robustness::mutate(&d, 17);
            }
        }
    }

    /// Fingerprints of what the reader reports for the corpus files (see `testkit::check_golden`).
    #[test]
    #[ignore = "needs the corpus"]
    fn golden_metadata_of_corpus_files() {
        testkit::check_golden(GOLDEN);
    }

    const GOLDEN: &[(&str, &str)] = &[
        ("Leica/C (Typ 112)/L1000006.RWL", "c9358f40de3a5062"),
        ("Leica/C-Lux/_1001076.RWL", "f586d4e5a2078339"),
        ("Leica/D-LUX (Typ 109)/pic_1-1.RWL", "6c2945cfe487a8a8"),
        ("Leica/D-LUX 4/L1010119.RWL", "db844965668b3746"),
        ("Leica/D-LUX 6/L1010139.RWL", "b4321efbb2808840"),
        ("Leica/D-Lux 7/L1023208.RWL", "73dde2417eeddbbf"),
        ("Leica/Digilux 3/L1050624.RAW", "718a6d2f6784182f"),
        ("Leica/Leica D-LUX 3/RAW_LEICA_DLUX3.RAW", "28de8d4d49001697"),
        ("Leica/Leica Digilux 2/RAW_LEICA_DIGILUX2_SRGB.RAW", "d6998c5a99bf2ef5"),
        ("Leica/V-LUX 1/RAW_LEICA_VLUX1.RAW", "a2da27baa1e4a3af"),
        ("Leica/V-Lux 4/L1040407.RWL", "329966f983336eef"),
        ("Panasonic/DC-FZ45/P1000336.RW2", "91b474f694bc73f1"),
        ("Panasonic/DC-G9/P1000951.RW2", "c459390ae627fc62"),
        ("Panasonic/DC-G90/PMAT0201.RW2", "bd5e0c767d2b0ccf"),
        ("Panasonic/DC-G9M2/P1000019.RW2", "8968fe17b7c77f92"),
        ("Panasonic/DC-GF10/P1010034.RW2", "b363811b6d8b5ae5"),
        ("Panasonic/DC-GH5/_T012010.RW2", "08ff8ab7451a12b4"),
        ("Panasonic/DC-GH5S/P1022091.RW2", "430a435c5245571e"),
        ("Panasonic/DC-GH7/P1002298.RW2", "4fe6d8bd3d2192ba"),
        ("Panasonic/DC-LX100M2/P1020079.RW2", "601b600dd714a88e"),
        ("Panasonic/DC-S1/P1033119.RW2", "5a765755af0f7181"),
        ("Panasonic/DC-S1M2/Panasonic_DC-S1M2_-_32.RW2", "06b53934b96ed82b"),
        ("Panasonic/DC-S1M2ES/3_2_P1022959.RW2", "b0230bd8bd9c27ff"),
        ("Panasonic/DC-S1R/P1000077.RW2", "10c22287ae9cc97a"),
        ("Panasonic/DC-S1RM2/P1000236.RW2", "5a511fc9798acead"),
        ("Panasonic/DC-S9/Panasonic_DC-S9_ISO200_3_2.RW2", "2dbdf48ffb4b0e4a"),
        ("Panasonic/DC-TZ90/P1010053.RW2", "d830f975072f504a"),
        ("Panasonic/DMC-FX150/1.3-Panasonic-DMC-FX150-1.78-P1040342.RW2", "8c6af3d4ee68c86f"),
        ("Panasonic/DMC-FZ100/P1080628.RW2", "901912167327c52b"),
        ("Panasonic/DMC-FZ1000/Panasonic_FZ1000_1_1.RW2", "2cf63c640cd01f4f"),
        ("Panasonic/DMC-FZ18/RAW_PANASONIC_LUMIX_FZ18.RAW", "aedfae6f54e5c2cc"),
        ("Panasonic/DMC-FZ200/P1040142.RW2", "945346dac15d919d"),
        ("Panasonic/DMC-FZ28/P1120637.RW2", "60cea37a576ad20e"),
        ("Panasonic/DMC-FZ30/RAW_PANASONIC_FZ30_SRGB.RAW", "650977a73d416e5b"),
        ("Panasonic/DMC-FZ38/Panasonic_DMC-FZ38_-16_9.RW2", "f3b4e27f76274061"),
        ("Panasonic/DMC-FZ50/P1250847.RAW", "a4d93d469a70e9ee"),
        ("Panasonic/DMC-FZ70/P1000836.RW2", "9c2d2204fbb2422b"),
        ("Panasonic/DMC-FZ72/Panasonic_FZ72_11.RW2", "9ff5ace9f005b6e6"),
        ("Panasonic/DMC-FZ8/P1240856.RAW", "771164cf5483274d"),
        ("Panasonic/DMC-G1/RAW_PANASONIC_G1.RW2", "3412779224e4bb33"),
        ("Panasonic/DMC-G10/P1030394.RW2", "cf358b79a817a825"),
        ("Panasonic/DMC-G2/LumixG21x1.RW2", "5131b6db5119c921"),
        ("Panasonic/DMC-G3/P1350400.RW2", "ccb7813e7596d05f"),
        ("Panasonic/DMC-G5/P1020264.RW2", "433cdae248a98861"),
        ("Panasonic/DMC-G6/Panasonic_DMC_G6_1x1.RW2", "dbcf11086c3c0bc0"),
        ("Panasonic/DMC-G7/171122_193328.RW2", "b401bf1579fcf810"),
        ("Panasonic/DMC-G70/Panasonic-DMC-G70-1_1-Kuerbis22.RW2", "243f68410643b4f7"),
        ("Panasonic/DMC-GF3/RAW_PANASONIC_GF3_16-9.RW2", "4395a82c5844904e"),
        ("Panasonic/DMC-GF5/RAW_PANASONIC_GF5_1x1.RW2", "107da7c6352f0d9c"),
        ("Panasonic/DMC-GF6/P1060738_16x9.RW2", "591bd07da3024072"),
        ("Panasonic/DMC-GH2/_1040800.RW2", "fc0e970a9e5711a3"),
        ("Panasonic/DMC-GH3/P1090819.RW2", "05fd20cf15ecfc5a"),
        ("Panasonic/DMC-GX1/_1160044.RW2", "75ca0d4790f1d8e8"),
        ("Panasonic/DMC-L10/RAW_PANASONIC_DMCL10.RAW", "bee2636623f4064f"),
        ("Panasonic/DMC-LF1/image-20170106_111000-1.rw2", "ece08d03af06d08a"),
        ("Panasonic/DMC-LX1/P1080763.RAW", "9831964407a4240f"),
        ("Panasonic/DMC-LX2/RAW_PANASONIC_LX2.RAW", "44bf365fae90f93e"),
        ("Panasonic/DMC-LX3/P1020593.RW2", "676d66b2c41ce23e"),
        ("Panasonic/DMC-LX5/LX5-1_1.rw2", "96b7e4fa6403ab95"),
        ("Panasonic/DMC-LX7/P1020372.RW2", "3b5f855741f70f5b"),
        ("Panasonic/DMC-TZ60/P1010792.RW2", "9f368a8f4deea79e"),
        ("Panasonic/DMC-TZ61/P1000167_aspect_ratio_16_9.RW2", "c84985672998469b"),
        ("Panasonic/DMC-TZ70/raw_panasonic_dmc_tz70_aspect_1_1.RW2", "bd9be2cb0f2455dd"),
        ("Panasonic/DMC-ZS40/P1000048.RW2", "e88ef13d32922fa8"),
    ];
}
