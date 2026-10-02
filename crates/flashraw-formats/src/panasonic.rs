//! Panasonic RW2 and Leica RWL.
//!
//! RW2 is a TIFF variant (`IIU\0`) whose IFD0 uses Panasonic's own tag numbers:
//! raw size (0x02/0x03), sensor borders (0x04–0x07), CFA (0x09), bit depth (0x0A),
//! linearity limits (0x0E–0x10), black levels (0x1C–0x1E), white balance
//! (0x24–0x26, older 0x11/0x12), raw format version (0x2D), embedded JPEG (0x2E),
//! format-8 coding parameters (0x39–0x48) and the payload offset (0x118).
//!
//! Payload codecs live in [`flashraw_codecs::panasonic`]. Geometry and calibration
//! `(0, 0, left_border + right_border, bottom_border + 12)` clipped to the buffer (plus
//! a small per-size table); the sensor borders themselves become [`FrameInfo::crop`].

use crate::common::*;
use flashraw_codecs::panasonic::{self as codec, BlockedStream, V8Decoder, V8Params, V8Strip};
use flashraw_core::tiff::{Entry, Ifd, Tiff, tags};
use flashraw_core::*;
use std::sync::Arc;

/// Magic-signature probe (`IIU\0`: RW2/RWL).
pub fn probe(data: &[u8]) -> bool {
    data.len() >= 8 && data.starts_with(b"IIU\0")
}

pub fn open<'a>(data: &'a [u8], opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    let tiff = Tiff::parse(data)?;
    open_tiff(data, tiff, opts)
}

const ALIASES: &[(&str, &[&str])] = &[
    ("DMC-FX150", &["DMC-FX180"]),
    ("DC-FZ1000M2", &["DC-FZ10002", "V-Lux 5"]),
    ("DMC-FZ1000", &["V-LUX (Typ 114)"]),
    ("DMC-FZ2500", &["DMC-FZ2000", "DMC-FZH1"]),
    ("DMC-FZ100", &["V-LUX 2"]),
    ("DMC-FZ150", &["V-LUX 3"]),
    ("DMC-FZ200", &["V-LUX 4"]),
    ("DMC-FZ300", &["DMC-FZ330"]),
    ("DMC-FZ35", &["DMC-FZ38"]),
    ("DMC-FZ40", &["DMC-FZ42", "DMC-FZ45", "DC-FZ40", "DC-FZ42", "DC-FZ45"]),
    ("DMC-FZ50", &["V-LUX 1", "V-LUX1"]),
    ("DMC-FZ70", &["DMC-FZ72"]),
    ("DC-FZ80", &["DC-FZ81", "DC-FZ82", "DC-FZ83", "DC-FZ85"]),
    ("DMC-LC1", &["DIGILUX 2", "Digilux 2", "DIGILUX2"]),
    ("DMC-LF1", &["C (Typ 112)"]),
    ("DC-LX100M2", &["D-Lux 7"]),
    ("DMC-LX100", &["D-LUX (Typ 109)", "D-Lux (Typ 109)"]),
    ("DMC-LX1", &["D-Lux2", "D-LUX2", "D-LUX 2"]),
    ("DMC-LX2", &["D-LUX 3", "D-LUX3"]),
    ("DMC-LX3", &["D-LUX 4"]),
    ("DMC-LX5", &["D-LUX 5"]),
    ("DMC-LX7", &["D-LUX 6"]),
    ("DMC-LX9", &["DMC-LX10", "DMC-LX15"]),
    ("DMC-ZS100", &["DMC-ZS110", "DMC-TZ100", "DMC-TZ101", "DMC-TZ110", "DMC-TX1"]),
    (
        "DC-ZS200",
        &[
            "DC-ZS220",
            "DC-TZ200",
            "DC-TZ202",
            "DC-TZ220",
            "DC-TZ200D",
            "DC-TZ202D",
            "DC-TZ220D",
            "DC-ZS200D",
            "DC-ZS220DDC-TX2",
            "C-Lux",
            "CAM-DC25",
        ],
    ),
    ("DMC-ZS40", &["DMC-TZ60", "DMC-TZ61"]),
    ("DMC-ZS50", &["DMC-TZ70", "DMC-TZ71"]),
    ("DMC-ZS60", &["DMC-TZ80", "DMC-TZ81", "DMC-TZ82", "DMC-TZ85"]),
    ("DC-ZS70", &["DC-TZ90", "DC-TZ91", "DC-TZ92", "DC-TZ93"]),
    ("DC-ZS80", &["DC-TZ95", "DC-TZ96", "DC-TZ97"]),
    ("DC-G100", &["DC-G110"]),
    ("DC-G99", &["DC-G90", "DC-G91", "DC-G95"]),
    ("DMC-G7", &["DMC-G70"]),
    ("DMC-G8", &["DMC-G80", "DMC-G81", "DMC-G85"]),
    ("DMC-GH4", &["AG-GH4", "CGO4"]),
    ("DC-GF10", &["DC-GF90", "DC-GX880"]),
    ("DC-GF9", &["DC-GX850", "DC-GX800"]),
    ("DMC-GM1", &["DMC-GM1S"]),
    ("DMC-GX85", &["DMC-GX80", "DMC-GX7MK2"]),
    ("DC-GX9", &["DC-GX7MK3"]),
    ("DMC-L1", &["DIGILUX 3", "DIGILUX3"]),
];

fn base_model(model: &str) -> &str {
    let m = model.trim();
    ALIASES.iter().find(|(_, al)| al.iter().any(|a| a.eq_ignore_ascii_case(m))).map(|(canon, _)| *canon).unwrap_or(m)
}

fn fallback_table(model: &str) -> Option<&'static flashraw_camera_db::FallbackMatrix> {
    let m = model.to_ascii_uppercase();
    flashraw_camera_db::MATRICES.iter().find(|t| {
        t.canonical_make.eq_ignore_ascii_case("Panasonic") && m.starts_with(&t.model_prefix.to_ascii_uppercase())
    })
}

/// (raw width, raw height, left, top, width delta, height delta).
const MARGINS: &[[i32; 6]] = &[
    [3130, 1743, 4, 0, -6, 0],
    [3130, 2055, 4, 0, -6, 0],
    [3130, 2319, 4, 0, -6, 0],
    [3170, 2103, 18, 0, -42, 20],
    [3170, 2367, 18, 13, -42, -21],
    [3177, 2367, 0, 0, -1, 0],
    [3304, 2458, 0, 0, -1, 0],
    [3330, 2463, 9, 0, -5, 0],
    [3330, 2479, 9, 0, -17, 4],
    [3370, 1899, 15, 0, -44, 20],
    [3370, 2235, 15, 0, -44, 20],
    [3370, 2511, 15, 10, -44, -21],
    [3690, 2751, 3, 0, -8, -3],
    [3710, 2751, 0, 0, -3, 0],
    [3724, 2450, 0, 0, 0, -2],
    [3770, 2487, 17, 0, -44, 19],
    [3770, 2799, 17, 15, -44, -19],
    [3880, 2170, 6, 0, -6, 0],
    [4060, 3018, 0, 0, 0, -2],
    [4290, 2391, 3, 0, -8, -1],
    [4330, 2439, 17, 15, -44, -19],
    [4508, 2962, 0, 0, -3, -4],
    [4508, 3330, 0, 0, -3, -6],
    [10480, 7794, 0, 0, -2, 0],
    [11560, 8680, 8, 8, -16, -16],
];

#[derive(Clone)]
enum Payload<'a> {
    V4(BlockedStream<'a>),
    V5(BlockedStream<'a>, u32),
    V6(&'a [u8], u32),
    V7(&'a [u8], u32),
    V8(Arc<V8Decoder>, Vec<V8Strip<'a>>),
    /// Early bodies without a payload version: 16-bit little-endian words, 12 bits left-aligned.
    Unpacked(&'a [u8]),
    /// Early bodies declared 12 bits per sample (FZ8/FZ18 `.RAW`): tightly packed, MSB first.
    Packed12(&'a [u8]),
}

struct Rw2<'a> {
    width: u32,
    height: u32,
    payload: Payload<'a>,
}

fn u16_tag(ifd: &Ifd, tag: u16) -> Option<u32> {
    ifd.get(tag).and_then(|e| e.u32(0))
}

/// Panasonic "counted" arrays: a u16 count followed by values of `size` bytes.
fn counted(e: &Entry, size: usize) -> Vec<u32> {
    let b = e.bytes();
    let n = b.get(..2).map(|c| u16::from_le_bytes([c[0], c[1]]) as usize).unwrap_or(0);
    (0..n)
        .filter_map(|i| {
            let o = 2 + i * size;
            let v = b.get(o..o + size)?;
            Some(match size {
                2 => u16::from_le_bytes([v[0], v[1]]) as u32,
                _ => u32::from_le_bytes([v[0], v[1], v[2], v[3]]),
            })
        })
        .collect()
}

fn v8_payload<'a>(data: &'a [u8], ifd: &Ifd<'a>, raw_w: u32, raw_h: u32) -> Result<Payload<'a>> {
    let e = |t: u16| ifd.get(t).ok_or_else(|| Error::invalid_metadata(format!("RW2 v8 tag {t:#x} missing")));
    let mut p = V8Params { clip: u16_tag(ifd, 0x3b).unwrap_or(0xffff), ..Default::default() };
    let slopes = counted(e(0x39)?, 4);
    // Tag 0x3A holds (unused, point) u16 pairs.
    let points = counted(e(0x3a)?, 4);
    for i in 0..6 {
        p.gamma_slopes[i] = slopes.get(i).copied().unwrap_or(0);
        p.gamma_points[i] = points.get(i).map(|v| v >> 16).unwrap_or(0);
    }
    for (i, t) in (0x3cu16..=0x3f).enumerate() {
        p.initial[i] = u16_tag(ifd, t).unwrap_or(0) as u16;
    }
    let codes = counted(e(0x40)?, 4);
    p.codes = codes.iter().map(|&v| ((v & 0xffff).min(16) as u8, ((v >> 16) & 0xfff) as u16)).collect();
    p.shift_down = counted(e(0x41)?, 2).iter().map(|&v| v.min(64) as u8).collect();
    // Strips form a grid: 0x42 columns by 0x43 rows (one row except on high-resolution shots).
    let count = u16_tag(ifd, 0x42).unwrap_or(0) as usize * u16_tag(ifd, 0x43).unwrap_or(1).max(1) as usize;
    if count == 0 || count > 64 {
        return Err(Error::corrupt(format!("RW2 v8 with {count} strips")));
    }
    let offsets = counted(e(0x44)?, 4);
    let lefts = counted(e(0x45)?, 4);
    let sizes = counted(e(0x46)?, 4);
    let widths = counted(e(0x47)?, 2);
    let heights = counted(e(0x48)?, 2);
    let mut strips = Vec::with_capacity(count);
    let mut area = 0u64;
    for i in 0..count {
        let get = |v: &Vec<u32>| v.get(i).copied().ok_or_else(|| Error::corrupt("RW2 v8 strip table too short"));
        let (off, bits) = (get(&offsets)? as usize, get(&sizes)? as u64);
        let len = bits.div_ceil(8) as usize;
        let sdata = flashraw_core::bytes::slice_at(data, off, len)?;
        // Line offset: x in the low 16 bits, y in the high 16 bits.
        let (pos, width, height) = (get(&lefts)?, get(&widths)?, get(&heights)?);
        let (left, top) = (pos & 0xffff, pos >> 16);
        if left + width > raw_w || top + height > raw_h {
            return Err(Error::corrupt("RW2 v8 strip outside the image"));
        }
        area += width as u64 * height as u64;
        strips.push(V8Strip { data: sdata, bits, left, top, width, height });
    }
    if area != raw_w as u64 * raw_h as u64 {
        return Err(Error::corrupt("RW2 v8 strips do not cover the image"));
    }
    Ok(Payload::V8(Arc::new(V8Decoder::new(&p)?), strips))
}

/// Open an RW2/RWL (or an older Panasonic TIFF-like RAW).
pub fn open_tiff<'a>(data: &'a [u8], tiff: Tiff<'a>, opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    let ifd = tiff
        .ifds
        .iter()
        .find(|i| i.has(0x118) || (i.has(0x02) && i.has(0x03) && i.has(tags::STRIP_OFFSETS)))
        .ok_or_else(|| Error::invalid_metadata("Panasonic file without raw IFD"))?;
    let mut meta = RawMetadata::default();
    flashraw_core::exif::read_ifd0(ifd, &mut meta);
    for i in tiff.ifds.iter().skip(1) {
        flashraw_core::exif::read_ifd0(i, &mut meta);
    }
    let raw_w = u16_tag(ifd, 0x02).unwrap_or(0);
    let raw_h = u16_tag(ifd, 0x03).unwrap_or(0);
    opts.limits.check_dims(raw_w, raw_h, 1)?;
    if raw_w < 22 || raw_h < 22 {
        return Err(Error::invalid_metadata(format!("RW2 raw size {raw_w}x{raw_h}")));
    }
    let bps = u16_tag(ifd, 0x0a).unwrap_or(12).clamp(8, 16);
    let version = u16_tag(ifd, 0x2d);
    if meta.exposure.iso.is_none() || meta.exposure.iso == Some(65535) {
        meta.exposure.iso = u16_tag(ifd, 0x17).filter(|&v| v != 0 && v != 65535).or_else(|| u16_tag(ifd, 0x37));
    }

    // Payload.
    let (payload, compression, mode_name) = if let Some(off) = ifd.u32(0x118) {
        let body = data
            .get(off as usize..)
            .filter(|b| !b.is_empty())
            .ok_or_else(|| Error::corrupt("RW2 payload offset beyond end of file"))?;
        let split = 0x2008;
        match version.unwrap_or(4) {
            4 => (Payload::V4(BlockedStream::new(body, split)?), Compression::Panasonic, "v4"),
            5 => (Payload::V5(BlockedStream::new(body, split)?, bps), Compression::PanasonicV5, "v5"),
            6 => (Payload::V6(body, bps), Compression::PanasonicV6, "v6"),
            7 => (Payload::V7(body, bps), Compression::PanasonicV7, "v7"),
            8 => (v8_payload(data, ifd, raw_w, raw_h)?, Compression::PanasonicV8, "v8"),
            v => return Err(Error::unsupported_compression("RW2", format!("raw format {v}"))),
        }
    } else {
        let off = ifd.u32(tags::STRIP_OFFSETS).ok_or_else(|| Error::invalid_metadata("RW2 without payload offset"))?;
        let body = data
            .get(off as usize..)
            .filter(|b| !b.is_empty())
            .ok_or_else(|| Error::corrupt("RW2 payload offset beyond end of file"))?;
        if raw_w >= 7 && (body.len() / (raw_w as usize * 8 / 7)) == raw_h as usize {
            (Payload::V4(BlockedStream::new(body, 0)?), Compression::Panasonic, "v4")
        } else if u16_tag(ifd, 0x0a).unwrap_or(0) == 12 || ifd.bps() == 12 {
            (Payload::Packed12(body), Compression::Uncompressed, "packed12")
        } else {
            (Payload::Unpacked(body), Compression::Uncompressed, "unpacked")
        }
    };
    let version = version.unwrap_or(match payload {
        Payload::V4(_) => 4,
        _ => 0,
    });
    meta.set_extra("panasonic_raw_format", version);
    meta.set_extra("encoding", mode_name);

    let (top_b, left_b, bottom_b, right_b) =
        (u16_tag(ifd, 0x04), u16_tag(ifd, 0x05), u16_tag(ifd, 0x06), u16_tag(ifd, 0x07));
    let mut width = match (left_b, right_b) {
        (Some(l), Some(r)) => (l + r) as i64,
        (Some(l), None) => l as i64,
        _ => 0,
    };
    let mut height = bottom_b.unwrap_or(0) as i64;
    if width == 0 {
        width = raw_w as i64;
    }
    if height == 0 {
        height = raw_h as i64;
    }
    height = (height + 12).min(raw_h as i64);
    let (mut left, mut top) = (0i64, 0i64);
    if let Some(m) = MARGINS.iter().find(|m| m[0] as u32 == raw_w && m[1] as u32 == raw_h) {
        left = m[2] as i64;
        top = m[3] as i64;
        width += m[4] as i64;
        height += m[5] as i64;
    }
    if top % 2 == 1 {
        top += 1;
        height -= 1;
    }
    if left % 2 == 1 {
        left += 1;
        width -= 1;
    }
    let mut info = FrameInfo::new(raw_w, raw_h, bps.min(16) as u8);
    let cl = |v: i64| v.clamp(0, u32::MAX as i64) as u32;
    info.active_area = Rect::new(cl(left), cl(top), cl(width), cl(height)).clamp_to(raw_w, raw_h);
    info.crop = match (top_b, left_b, bottom_b, right_b) {
        (Some(t), Some(l), Some(b), Some(r)) if r > l && b > t => {
            Rect::new(l, t, r - l, b - t).intersect(&info.active_area).unwrap_or(info.active_area)
        }
        _ => info.active_area,
    };
    info.compression = compression;
    info.orientation = Orientation::from_exif(ifd.u32(tags::ORIENTATION).unwrap_or(1));

    // CFA (tag 0x09: 1 = RGGB, 2 = GRBG, 3 = GBRG, 4 = BGGR at the buffer origin).
    let mut cfa_code = u16_tag(ifd, 0x09).unwrap_or(1);
    if base_model(&meta.model).eq_ignore_ascii_case("DC-LX100M2") && raw_w == 4816 && raw_h == 3568 && cfa_code == 3 {
        cfa_code = 4;
    }
    let cfa = match cfa_code {
        2 => CfaPattern::GRBG,
        3 => CfaPattern::GBRG,
        4 => CfaPattern::BGGR,
        _ => CfaPattern::RGGB,
    };
    info.cfa = Some(cfa);
    info.layout = SensorLayout::Bayer;

    let model = base_model(&meta.model).to_string();
    let table = fallback_table(&model);
    let mode = guess_mode(raw_w, raw_h);
    let cam = db_lookup(&mut meta, mode);
    meta.set_extra("aspect", mode);

    // Black level per CFA colour.
    let pana_black = [u16_tag(ifd, 0x1c), u16_tag(ifd, 0x1d), u16_tag(ifd, 0x1e)];
    let modern = version >= 6;
    let mut known = Known { cfa: true, crop: true, active_area: true, black: true, white: true, ..Default::default() };
    let per_colour = |rgb: [f32; 3]| -> BlackLevel {
        let v: Vec<f32> = (0..4)
            .map(|i| match cfa.color_at(i % 2, i / 2) {
                CfaColor::Red => rgb[0],
                CfaColor::Blue => rgb[2],
                _ => rgb[1],
            })
            .collect();
        BlackLevel::pattern(2, 2, v)
    };
    if modern {
        let b = pana_black.map(|v| v.unwrap_or(0) as f32);
        info.black_level = per_colour(b);
    } else if pana_black.iter().all(|v| v.is_some_and(|v| v > 0)) {
        let add = if version == 4 { 15.0 } else { 0.0 };
        info.black_level = per_colour(pana_black.map(|v| v.unwrap_or(0) as f32 + add));
    } else if let Some(t) = table.filter(|t| t.black != 0) {
        info.black_level = BlackLevel::uniform(t.black.unsigned_abs() as f32);
    } else {
        known.black = false;
    }

    // White level.
    let lin: Vec<u32> = (0x0eu16..=0x10)
        .filter_map(|t| u16_tag(ifd, t))
        .map(|v| match v {
            16383 => 16383 - 64,
            4095 => 4095 - 16,
            v => v,
        })
        .collect();
    let bits_max = if bps >= 16 { 65535 } else { (1u32 << bps) - 1 };
    info.white_level = WhiteLevel::uniform(if modern && !lin.is_empty() {
        lin.iter().copied().max().unwrap_or(bits_max)
    } else if let Some(t) = table.filter(|t| t.white > 0) {
        t.white as u32
    } else {
        bits_max
    });

    // White balance.
    let wb = match (u16_tag(ifd, 0x24), u16_tag(ifd, 0x25), u16_tag(ifd, 0x26)) {
        (Some(r), Some(g), Some(b)) => WhiteBalance::from_multipliers(r as f32, g as f32, b as f32, g as f32),
        _ => match (u16_tag(ifd, 0x11), u16_tag(ifd, 0x12)) {
            (Some(r), Some(b)) => WhiteBalance::from_multipliers(r as f32, 256.0, b as f32, 256.0),
            _ => None,
        },
    };
    info.white_balance = wb;

    if let Some(cm) = table.and_then(|t| {
        ColorMatrix::new(Illuminant::D65, 3, t.color_matrix.iter().take(9).map(|&v| v as f32 / 10000.0).collect())
    }) {
        info.color_matrices = ColorMatrices { color: vec![cm], source: ColorSource::CameraDb, ..Default::default() };
        known.color = true;
    }
    apply_db(&mut info, &meta, cam, known);

    // Previews: the embedded full-size JPEG (0x2E) and any TIFF thumbnails.
    meta.previews = tiff_previews(data, 0, &tiff, info.orientation);
    if let Some(e) = ifd.get(0x2e) {
        push_jpeg(data, e.value_pos, e.bytes().len(), info.orientation, &mut meta.previews);
    }
    classify_previews(&mut meta.previews);

    let rw2 = Arc::new(Rw2 { width: raw_w, height: raw_h, payload });
    let r = rw2.clone();
    let mut dec = SimpleDecoder::new(
        RawFormat::Rw2,
        meta,
        info,
        Box::new(move |ctx| {
            let mut img = RawImage::new_u16(r.width, r.height, 1, &ctx.limits)?;
            ctx.profiler.time(Stage::EntropyDecode, || r.decode(img.u16_mut(), ctx))?;
            Ok(img)
        }),
    );
    let r2 = rw2.clone();
    dec.region = Some(Box::new(move |ctx, rect| {
        let full = Rect::full(r2.width, r2.height);
        let rect =
            rect.intersect(&full).ok_or_else(|| Error::InvalidArgument(format!("region {rect} outside image")))?;
        ctx.profiler.time(Stage::EntropyDecode, || r2.decode_region(rect, ctx))
    }));
    dec.random_access = match &rw2.payload {
        Payload::V8(..) => RandomAccess::Blocks,
        Payload::V4(_) if raw_w as usize % codec::V4_PIXELS != 0 => RandomAccess::Sequential,
        _ => RandomAccess::Rows,
    };
    if let Payload::V8(_, strips) = &rw2.payload {
        dec.units = strips
            .iter()
            .map(|s| DecodeUnit {
                rect: Rect::new(s.left, s.top, s.width, s.height),
                offset: (s.data.as_ptr() as usize - data.as_ptr() as usize) as u64,
                length: s.data.len() as u64,
            })
            .collect();
    }
    Ok(Box::new(dec))
}

fn guess_mode(w: u32, h: u32) -> &'static str {
    let ratio = w as f32 / h.max(1) as f32;
    let mut best = ("16:9", (ratio - 16.0 / 9.0).abs());
    for (name, r) in [("3:2", 1.5f32), ("4:3", 4.0 / 3.0), ("1:1", 1.0)] {
        let d = (ratio - r).abs();
        if d < best.1 {
            best = (name, d);
        }
    }
    best.0
}

impl Rw2<'_> {
    /// Decode only the rows of `rect` (row-independent formats), then keep its columns.
    fn decode_region(&self, rect: Rect, ctx: &DecodeContext) -> Result<RawImage> {
        let (w, h) = (self.width as usize, self.height as usize);
        let (y0, rows) = (rect.y as usize, rect.height as usize);
        let par = ctx.parallel;
        let mut band = RawImage::new_u16(self.width, rect.height, 1, &ctx.limits)?;
        let out = band.u16_mut();
        match &self.payload {
            Payload::V4(s) if w % codec::V4_PIXELS == 0 => codec::decode_v4_rows(s, w, h, y0, out, par)?,
            Payload::V5(s, bits) => codec::decode_v5_rows(s, *bits, w, h, y0, out, par)?,
            Payload::V6(d, bits) => codec::decode_v6_rows(d, *bits, w, h, y0, out, par)?,
            Payload::V7(d, bits) => codec::decode_v7_rows(d, *bits, w, h, y0, out, par)?,
            Payload::Packed12(d) => unpack_packed12(d, w, h, y0, rows, out, par, ctx.simd)?,
            Payload::Unpacked(d) => {
                if d.len() < w * h * 2 {
                    return Err(Error::corrupt("RW2 unpacked payload truncated"));
                }
                let src = &d[y0 * w * 2..(y0 + rows) * w * 2];
                for (o, s) in out.iter_mut().zip(src.chunks_exact(2)) {
                    *o = u16::from_le_bytes([s[0], s[1]]) >> 4;
                }
            }
            _ => {
                // Sequential streams and format-8 strips (full image height): decode all.
                let mut img = RawImage::new_u16(self.width, self.height, 1, &ctx.limits)?;
                self.decode(img.u16_mut(), ctx)?;
                return img.crop(rect);
            }
        }
        band.crop(Rect::new(rect.x, 0, rect.width, rect.height))
    }

    fn decode(&self, out: &mut [u16], ctx: &DecodeContext) -> Result<()> {
        let (w, h) = (self.width as usize, self.height as usize);
        let par = ctx.parallel;
        match &self.payload {
            Payload::V4(s) => codec::decode_v4(s, w, h, out, par),
            Payload::V5(s, bits) => codec::decode_v5(s, *bits, w, h, out, par),
            Payload::V6(d, bits) => codec::decode_v6(d, *bits, w, h, out, par),
            Payload::V7(d, bits) => codec::decode_v7(d, *bits, w, h, out, par),
            Payload::V8(dec, strips) => dec.decode(strips, out, w, h, par),
            Payload::Packed12(d) => unpack_packed12(d, w, h, 0, h, out, par, ctx.simd),
            Payload::Unpacked(d) => {
                if d.len() < w * h * 2 {
                    return Err(Error::corrupt("RW2 unpacked payload truncated"));
                }
                for (o, s) in out[..w * h].iter_mut().zip(d.chunks_exact(2)) {
                    *o = u16::from_le_bytes([s[0], s[1]]) >> 4;
                }
                Ok(())
            }
        }
    }
}

/// Early 12-bit bodies (FZ8/FZ18): LSB-first (little-endian) packed rows, either tight or with one
/// `load_flags & 1` expects. The layout is chosen from the payload size.
#[allow(clippy::too_many_arguments)]
fn unpack_packed12(
    d: &[u8],
    w: usize,
    h: usize,
    y0: usize,
    rows: usize,
    out: &mut [u16],
    par: bool,
    simd: bool,
) -> Result<()> {
    let tight = w * 3 / 2;
    let padded = tight * 16 / 15;
    if d.len() < padded * h {
        let src = d.get(y0 * tight..).unwrap_or(&[]);
        let layout = flashraw_codecs::PackedLayout::tight(w, rows, 12, PackOrder::LsbBytes);
        return flashraw_codecs::unpack(src, layout, out, w, par, simd, false);
    }
    if d.len() < padded * (y0 + rows) {
        return Err(Error::corrupt("RW2 packed payload truncated"));
    }
    par::for_each_row_chunk(&mut out[..w * rows], w, 32, par, |first, chunk| {
        let mut row = vec![0u8; tight + 16];
        for (i, o) in chunk.chunks_mut(w).enumerate() {
            let src = &d[(y0 + first + i) * padded..(y0 + first + i + 1) * padded];
            // Drop the padding byte that follows every 15 data bytes.
            let mut n = 0;
            for g in src.chunks(16) {
                let take = g.len().min(15);
                row[n..n + take].copy_from_slice(&g[..take]);
                n += take;
            }
            flashraw_codecs::unpack_row(&row[..tight.min(n)], o, 12, PackOrder::LsbBytes, simd);
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal RW2: IFD0 with Panasonic tags and a format-7 (14-bit, 9 px / 16 bytes) payload.
    fn synthetic_rw2(w: u16, h: u16, vals: &[u16]) -> Vec<u8> {
        let mut payload = Vec::new();
        for row in vals.chunks(w as usize) {
            for px in row.chunks(9) {
                let mut v: u128 = 0;
                for (k, &p) in px.iter().enumerate() {
                    v |= (p as u128) << (14 * k);
                }
                payload.extend_from_slice(&v.to_le_bytes());
            }
        }
        let entries: Vec<(u16, u16, u32)> = vec![
            (0x02, 3, w as u32),
            (0x03, 3, h as u32),
            (0x04, 3, 2),
            (0x05, 3, 4),
            (0x06, 3, h as u32 - 2),
            (0x07, 3, w as u32 - 4),
            (0x09, 3, 2),
            (0x0a, 3, 14),
            (0x0e, 3, 16383),
            (0x0f, 3, 16383),
            (0x10, 3, 16383),
            (0x1c, 3, 510),
            (0x1d, 3, 512),
            (0x1e, 3, 514),
            (0x24, 3, 500),
            (0x25, 3, 256),
            (0x26, 3, 400),
            (0x2d, 3, 7),
            (0x112, 3, 6),
        ];
        let n = entries.len() + 1;
        let ifd_end = 8 + 2 + 12 * n + 4;
        let mut f = b"IIU\0".to_vec();
        f.extend(8u32.to_le_bytes());
        f.extend((n as u16).to_le_bytes());
        for (t, ty, v) in entries {
            f.extend(t.to_le_bytes());
            f.extend(ty.to_le_bytes());
            f.extend(1u32.to_le_bytes());
            f.extend(v.to_le_bytes());
        }
        f.extend(0x118u16.to_le_bytes());
        f.extend(4u16.to_le_bytes());
        f.extend(1u32.to_le_bytes());
        f.extend((ifd_end as u32).to_le_bytes());
        f.extend(0u32.to_le_bytes());
        f.extend(payload);
        f
    }

    fn decode(data: &[u8]) -> Result<(FrameInfo, Vec<u16>)> {
        let d = crate::open(data, &OpenOptions::default())?;
        let p = Profiler::new();
        let img = d.decode(&DecodeContext::new(&p))?;
        Ok((d.frame_info().clone(), img.u16().to_vec()))
    }

    #[test]
    fn synthetic_v7_rw2() {
        let (w, h) = (72u16, 32u16);
        let vals: Vec<u16> = (0..w as usize * h as usize).map(|i| (i * 131 % 16384) as u16).collect();
        let f = synthetic_rw2(w, h, &vals);
        assert!(probe(&f));
        let (info, px) = decode(&f).unwrap();
        assert_eq!(px, vals);
        assert_eq!(info.cfa, Some(CfaPattern::GRBG));
        assert_eq!(info.white_level.max(), 16383 - 64);
        assert_eq!(info.orientation, Orientation::Rotate90);
        assert_eq!(info.crop, Rect::new(4, 2, 64, 28));
        assert_eq!(info.active_area, Rect::new(0, 0, 72, 32));
        // Black per colour on a GRBG pattern.
        assert_eq!(info.black_level.values, vec![512.0, 510.0, 514.0, 512.0]);
        let wb = info.white_balance.unwrap();
        assert!((wb.coeffs[0] - 500.0 / 256.0).abs() < 1e-4);
    }

    #[test]
    fn synthetic_rw2_truncation_and_corruption_never_panic() {
        let (w, h) = (72u16, 32u16);
        let vals: Vec<u16> = (0..w as usize * h as usize).map(|i| i as u16 & 0x3fff).collect();
        let f = synthetic_rw2(w, h, &vals);
        for cut in [0, 7, 30, 200, f.len() / 2, f.len() - 1] {
            assert!(decode(&f[..cut]).is_err(), "truncated at {cut} must fail");
        }
        let mut x = 0x1234_5678u32;
        for i in 0..400 {
            let mut g = f.clone();
            for _ in 0..3 {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                let pos = (x as usize) % g.len().min(64 + i);
                g[pos] = (x >> 8) as u8;
            }
            let _ = decode(&g);
        }
    }

    #[test]
    fn aspect_mode_guess() {
        assert_eq!(guess_mode(4000, 3000), "4:3");
        assert_eq!(guess_mode(6000, 4000), "3:2");
        assert_eq!(guess_mode(1920, 1080), "16:9");
        assert_eq!(guess_mode(3000, 3000), "1:1");
        assert_eq!(base_model("D-LUX 4"), "DMC-LX3");
        assert_eq!(base_model("DC-GH5"), "DC-GH5");
    }

    /// Corpus files (when present): truncated or corrupted copies must fail cleanly.
    #[test]
    fn corpus_malformed_inputs() {
        let corpus = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus");
        let files = [
            "Leica/D-LUX 5/L1010451.RWL",
            "Panasonic/DC-GH5M2/Panasonic_-_DC-GH5M2_-_1:1.RW2",
            "Panasonic/DC-S5/P1047510.RW2",
            "Panasonic/DC-GH6/4-3.RW2",
        ];
        for f in files {
            let Ok(data) = std::fs::read(corpus.join(f)) else { continue };
            assert!(decode(&data).is_ok(), "{f} should decode");
            for frac in [0.0001, 0.01, 0.5, 0.9] {
                let cut = (data.len() as f64 * frac) as usize;
                assert!(decode(&data[..cut]).is_err(), "{f} truncated to {cut} bytes must fail");
            }
            let mut g = data.clone();
            let n = g.len();
            for k in 0..64 {
                let pos = n / 2 + k * 977 % (n / 3);
                g[pos] ^= 0xa5;
            }
            let _ = decode(&g);
        }
    }
}
