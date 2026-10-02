//! Nikon NEF / NRW.
//!
//! Payload variants (selected from the raw IFD's compression, size and makernote,
//!
//! | variant | detection | codec |
//! |---|---|---|
//! | Huffman (lossy / lossless, 12/14-bit) | compression 34713, other cases fail | [`nikon_huffman`] |
//! | packed 12-bit MSB | compression 1, 12 bps (multi-strip D1..D4 era) | [`nikon_packed`] |
//! | packed 12-bit LSB ("80" flags) | 34713 with `w*h*3/2` bytes | [`nikon_packed`] |
//! | packed + pad byte per 10 samples | 34713 with `ceil(w/10)*16*h` bytes (D100) | [`nikon_packed`] |
//! | interlaced fields | Coolpix E-series NEF | [`nikon_packed`] |
//! | Msb32 packed | Coolpix P / B700 NRW | [`nikon_packed`] |
//! | 16-bit words | compression 1 with 14 bps, or `w*h*2` bytes | big/little-endian words |
//! | 14-bit LSB, rows padded to 16 bytes | Z / D6 uncompressed 14-bit | [`packed`] |
//! | 12-bit LSB, rows padded to 16 bytes | Z / D6 uncompressed 12-bit | [`packed`] |
//! | small/medium sNEF (YCbCr 4:2:2, 12-bit) | `w*h*3` bytes | YCbCr → RGB (3 samples/pixel) |
//! | high efficiency (TicoRAW / JPEG XS) | JPEG XS SOC marker at the payload start | unsupported |
//!
//! visible-area trims, makernote black in 14-bit units scaled to 12-bit files).

mod crypt;
mod makernote;

use crate::common::*;
use flashraw_codecs::nikon_huffman::{self, NikonMeta};
use flashraw_codecs::nikon_packed::{self, PackedSpec};
use flashraw_codecs::packed::{self, PackedLayout};
use flashraw_core::tiff::{Ifd, Tiff, tags};
use flashraw_core::*;
use makernote::MakerNote;
use std::sync::Arc;

/// Magic-signature probe (NEF/NRW are TIFF-based and dispatched by make).
pub fn probe(_data: &[u8]) -> bool {
    false
}

pub fn open<'a>(_data: &'a [u8], _opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    Err(Error::UnsupportedFormat("NEF files are TIFF based; use open_tiff".into()))
}

/// How the payload is stored.
#[derive(Debug, Clone)]
enum Payload {
    Huffman { meta: Arc<NikonMeta>, rows: usize },
    Packed(PackedSpec),
    /// 16-bit words shifted right by `shift`.
    Words { order: ByteOrder, shift: u32 },
    /// Little-endian packed rows padded to 16 bytes (Z-series / D6 uncompressed).
    PaddedLsb { bits: u32, row_stride: usize },
    /// Small/medium NEF: YCbCr 4:2:2, 12-bit samples.
    Ycbcr,
    /// Nikon Scan (Coolscan) RGB or grey TIFF data.
    Coolscan { samples: u32, bits: u32, order: ByteOrder },
    /// "High efficiency" TicoRAW (JPEG XS based); not decodable.
    HighEfficiency,
}

struct NefRaw<'a> {
    /// past the declared strip length).
    src: &'a [u8],
    width: usize,
    height: usize,
    payload: Payload,
}

/// Open a NEF/NRW (or Nikon Scan NEF).
pub fn open_tiff<'a>(data: &'a [u8], tiff: Tiff<'a>, opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    let ifd0 = tiff.ifds.first().ok_or_else(|| Error::corrupt("NEF without IFD0"))?;
    let mut meta = RawMetadata::default();
    flashraw_core::exif::read_ifd0(ifd0, &mut meta);
    if meta.make.is_empty() {
        meta.make = tiff.all_ifds().into_iter().find_map(|i| i.string(tags::MAKE)).unwrap_or_default();
    }
    if meta.model.is_empty() {
        meta.model = tiff.all_ifds().into_iter().find_map(|i| i.string(tags::MODEL)).unwrap_or_default();
    }
    let model = strip_make_prefix(&meta.model, "NIKON");
    let model_uc = model.to_ascii_uppercase();
    let mn =
        tiff.find_entry(tags::MAKER_NOTE).map(|e| makernote::parse(data, e, tiff.order, &model)).unwrap_or_default();
    if meta.exposure.iso.is_none() {
        meta.exposure.iso = mn.iso;
    }
    if meta.serial.is_none() {
        meta.serial = mn.serial.clone();
    }
    if meta.lens.focal_range.is_none() {
        if let Some(l) = mn.lens_spec {
            meta.lens.focal_range = Some((l[0], l[1]));
            if l[2] > 0.0 {
                meta.lens.aperture_range = Some((l[2], l[3]));
            }
        }
    }

    let raw = select_raw_ifd(&tiff).ok_or_else(|| Error::invalid_metadata("NEF has no raw image IFD"))?;
    let raw_w = raw.width();
    let raw_h = raw.height();
    opts.limits.check_dims(raw_w, raw_h, 1)?;
    let bps = raw.bps();
    let compression = raw.compression();
    let strips = strips(raw);
    let (offset, bytes) = *strips.first().ok_or_else(|| Error::invalid_metadata("NEF raw IFD without strips"))?;
    if offset >= data.len() {
        return Err(Error::corrupt_at("NEF payload offset beyond end of file", offset as u64));
    }
    let src = &data[offset..];
    let software = meta.software.clone().unwrap_or_default();
    let samples = raw.u32(tags::SAMPLES_PER_PIXEL).unwrap_or(1);

    let (w64, h64, b64) = (raw_w as u64, raw_h as u64, bytes as u64);
    let is_coolscan = software.starts_with("Nikon Scan")
        || model_uc.contains("COOLSCAN")
        || (bps == 8 && compression == 1 && raw.u32(tags::PHOTOMETRIC) != Some(32803));
    let mut width = raw_w as usize;
    let height = raw_h as usize;
    let mut packed_flags: Option<u32> = None;
    let mut huffman = false;
    let mut payload = if is_coolscan {
        Payload::Coolscan { samples, bits: bps, order: tiff.order }
    } else {
        match compression {
            34713 => {
                if (w64 + 9) / 10 * 16 * h64 == b64 {
                    packed_flags = Some(1);
                    Payload::Packed(PackedSpec { width, height, bits: 12, flags: 1, field2_offset: None })
                } else if w64 * h64 * 3 == b64 * 2 {
                    let flags = if meta.model.starts_with('N') { 80 } else { 0 };
                    packed_flags = Some(flags);
                    Payload::Packed(PackedSpec { width, height, bits: bps, flags, field2_offset: None })
                } else if w64 * h64 * 3 == b64 {
                    Payload::Ycbcr
                } else if w64 * h64 * 2 == b64 {
                    Payload::Words { order: ByteOrder::Big, shift: 4 }
                } else if (w64 * 3 / 2).div_ceil(16) * 16 * h64 == b64 {
                    Payload::PaddedLsb { bits: 12, row_stride: ((width * 3 / 2) + 15) / 16 * 16 }
                } else if src.get(..4) == Some(&[0xff, 0x10, 0xff, 0x50]) {
                    // JPEG XS SOC + CAP markers: Nikon "high efficiency" (TicoRAW).
                    Payload::HighEfficiency
                } else {
                    huffman = true;
                    Payload::HighEfficiency // placeholder, replaced below
                }
            }
            1 | 0 => match bps {
                8 => Payload::Words { order: tiff.order, shift: 0 }, // replaced below (8-bit)
                12 => {
                    // A1000 stores 12-bit data in 16-bit words.
                    if model_uc.starts_with("COOLPIX A1000") && b64 == w64 * h64 * 2 {
                        Payload::Words { order: tiff.order, shift: 0 }
                    } else {
                        let flags = if raw.u32(tags::PHOTOMETRIC) == Some(2) { 6 } else { 0 };
                        packed_flags = Some(flags);
                        Payload::Packed(PackedSpec { width, height, bits: 12, flags, field2_offset: None })
                    }
                }
                14 | 16 => Payload::Words { order: tiff.order, shift: 0 },
                b => return Err(Error::unsupported_compression("NEF", format!("uncompressed {b}-bit"))),
            },
            c => return Err(Error::unsupported_compression("NEF", format!("TIFF compression {c}"))),
        }
    };
    if bps == 8 && !is_coolscan {
        return Err(Error::unsupported_compression("NEF", "8-bit uncompressed"));
    }
    // Z-series and D6 uncompressed 14-bit: LSB-first, rows padded to 16 bytes.
    let model_is_z_or_d6 = model_uc.starts_with('Z') || model_uc == "D6";
    if model_is_z_or_d6 && ((width * 7 / 4) as f32 / 16.0).ceil() as u64 * 16 * h64 == b64 {
        payload = Payload::PaddedLsb { bits: 14, row_stride: (((width * 7 / 4) as f32 / 16.0).ceil() as usize) * 16 };
        huffman = false;
    }

    let mut geo = Geometry { left: 0, top: 0, width: raw_w, height: raw_h, raw_width: raw_w };
    let iso = meta.exposure.iso.unwrap_or(0);
    let mut black_override: Option<u32> = None;
    let mut max_override: Option<u32> = None;
    let mut cfa_override: Option<CfaPattern> = None;
    let model_matches = |m: &str| model_uc == m;
    let mut wb_is_analog = false;
    let raw_width_4032 = raw_w == 4032;
    if model_matches("D1") {
        wb_is_analog = true;
    } else if model_matches("D1X") {
        geo.width -= 4;
    } else if ["D40X", "D60", "D80", "D3000"].iter().any(|m| model_matches(m)) {
        geo.height -= 3;
        geo.width -= 4;
    } else if ["D3", "D3S", "D700"].iter().any(|m| model_matches(m)) {
        geo.width -= 4;
        geo.left = 2;
    } else if model_matches("D3100") {
        geo.width -= 28;
        geo.left = 6;
    } else if model_matches("D5000") || model_matches("D90") {
        geo.width -= 42;
    } else if ["D5100", "D7000", "COOLPIX A"].iter().any(|m| model_matches(m)) {
        geo.width -= 44;
    } else if ["D3200", "D600", "D610"].iter().any(|m| model_matches(m)) || model_uc.starts_with("D800") {
        geo.width -= 46;
    } else if model_matches("D4") || model_matches("DF") {
        geo.width -= 52;
        geo.left = 2;
    } else if model_matches("D500") {
    } else if model_uc.starts_with("D40") || model_uc.starts_with("D50") || model_uc.starts_with("D70") {
        geo.width -= 1;
    } else if model_matches("D100") {
        if packed_flags.is_some_and(|f| f != 0) {
            geo.width += 3;
            geo.raw_width = geo.width + 3;
        }
    } else if model_matches("D200") {
        geo.left = 1;
        geo.width -= 4;
        cfa_override = Some(CfaPattern::GRBG);
    } else if model_uc.starts_with("D2H") {
        geo.left = 6;
        geo.width -= 14;
    } else if model_uc.starts_with("D2X") {
        geo.width -= if geo.width == 3264 { 32 } else { 8 };
    } else if model_uc.starts_with("D300") {
        geo.width -= 32;
    } else if raw_width_4032 {
        // P7700, P7800, P330, P340
        if model_uc == "COOLPIX P7700" || model_uc == "COOLPIX P7800" {
            max_override = Some(65504);
            if let Payload::Words { shift, .. } = &mut payload {
                *shift = 0;
            }
        } else if model_uc == "COOLPIX P340" {
            if let Payload::Words { shift, .. } = &mut payload {
                *shift = 0;
            }
            if let Payload::Packed(s) = &mut payload {
                s.flags = 0;
            }
        }
    } else if model_uc.starts_with("COOLPIX P") || model_uc.starts_with("COOLPIX B700") {
        if let Payload::Packed(s) = &mut payload {
            s.flags = 24;
        }
        if model_uc.starts_with("COOLPIX P") {
            cfa_override = Some(CfaPattern::RGGB);
            if model_uc.as_bytes().get(9) == Some(&b'7') && (iso >= 400 || iso == 0) && !software.contains("V1.2") {
                black_override = Some(255);
            }
        }
    } else if model_uc.starts_with("1 ") {
        geo.height -= 2;
    } else if model_uc.starts_with('E') {
        // Coolpix E-series NEF: rows stored as two interlaced fields.
        if let Payload::Packed(s) = &mut payload {
            s.flags |= 2;
        }
    }
    if geo.left % 2 == 1 {
        geo.left += 1;
        geo.width -= 1;
    }
    if geo.top % 2 == 1 {
        geo.top += 1;
        geo.height -= 1;
    }

    // Final payload description with the (possibly widened) buffer.
    width = geo.raw_width as usize;
    if let Payload::Packed(s) = &mut payload {
        s.width = width;
    }
    if huffman {
        let lin = mn.lin_offset.ok_or_else(|| Error::invalid_metadata("compressed NEF without linearization table"))?;
        let order = mn.order.unwrap_or(tiff.order);
        let nm = NikonMeta::parse(data.get(lin..).unwrap_or(&[]), order, bps)?;
        let rows = geo.height.min(raw_h) as usize;
        payload = Payload::Huffman { meta: Arc::new(nm), rows };
    }

    // --- Levels ---
    let bits_out: u8 = match &payload {
        Payload::Ycbcr => 14,
        Payload::Coolscan { .. } => 16,
        _ => bps.min(16) as u8,
    };
    meta.canonical_make = "Nikon".into();
    meta.canonical_model = table_model_name(&model);
    let mode = mode_name(&payload, bps, &mn);
    let cam = db_lookup(&mut meta, &mode);
    let fallback = flashraw_camera_db::find_matrix("Nikon", &table_model_name(&model));

    // Black: makernote (14-bit units, RGGB) → camera table → 12-bit scaling.
    let mut black = [0u32; 4]; // R, G1, G2, B
    let mut base_black = 0u32;
    if let Some(b) = mn.black {
        let m = *b.iter().min().unwrap_or(&0) as u32;
        base_black = m;
        for i in 0..4 {
            black[i] = b[i] as u32 - m;
        }
    }
    if let Some(nb) = mn.nrw_black {
        base_black = nb;
    }
    if let Some(bo) = black_override {
        base_black = bo;
    }
    let mut maximum: u32 = max_override.unwrap_or(0);
    if let Some(f) = fallback {
        let rblack = base_black + black.iter().sum::<u32>() / 4;
        if f.black > 0 {
            base_black = f.black as u32;
            black = [0; 4];
        } else if f.black < 0 && rblack == 0 {
            base_black = (-f.black) as u32;
            black = [0; 4];
        }
        if f.white > 0 {
            maximum = f.white as u32;
        }
    }
    if maximum == 0 {
        maximum = (1u32 << bps.clamp(1, 16)) - 1;
    }
    let scaled_12bit = matches!(payload, Payload::Huffman { .. } | Payload::Packed(_))
        || matches!(payload, Payload::PaddedLsb { bits: 12, .. });
    if scaled_12bit && bps == 12 && !model_uc.starts_with("COOLPIX") {
        maximum = 4095;
        base_black /= 4;
        for b in black.iter_mut() {
            *b /= 4;
        }
    }
    if matches!(payload, Payload::Ycbcr) {
        maximum = 16383;
        base_black = 0;
        black = [0; 4];
    }
    if matches!(payload, Payload::Coolscan { .. }) {
        // Scans are output in the full 16-bit range (8-bit data is expanded).
        maximum = 65535;
        base_black = 0;
        black = [0; 4];
    }

    // --- FrameInfo ---
    let (cpp, layout_override) = match &payload {
        Payload::Ycbcr => (3, Some(SensorLayout::Linear)),
        Payload::Coolscan { samples, .. } => {
            (*samples, Some(if *samples >= 3 { SensorLayout::Linear } else { SensorLayout::Monochrome }))
        }
        _ => (1, None),
    };
    let mut info = FrameInfo::new(geo.raw_width, raw_h, bits_out);
    info.cpp = cpp;
    info.compression = match &payload {
        Payload::Huffman { .. } => Compression::NikonHuffman,
        Payload::HighEfficiency => Compression::NikonHighEfficiency,
        _ => Compression::Uncompressed,
    };
    info.active_area = Rect::new(geo.left, geo.top, geo.width, geo.height);
    info.crop = info.active_area;
    info.white_level = WhiteLevel::uniform(maximum);
    info.orientation = file_orientation(&tiff);
    if let Some(l) = layout_override {
        info.layout = l;
        info.cfa = None;
    } else {
        let db_cfa = cam.and_then(|c| c.cfa).and_then(parse_db_cfa);
        // Interlaced Coolpix E-series files describe the CFA of the stored (field
        // ordered) rows; the database pattern describes the de-interlaced sensor.
        let interlaced = matches!(&payload, Payload::Packed(s) if s.flags & 2 != 0);
        let cfa = cfa_override
            .or(if interlaced { db_cfa } else { None })
            .or_else(|| tiff_ep_cfa(raw))
            .or(db_cfa)
            .unwrap_or(CfaPattern::RGGB);
        info.cfa = Some(cfa);
        info.layout = layout_for(&cfa);
        // Black per CFA site: RGGB-ordered values mapped onto the pattern.
        let mut pat = [0f32; 4];
        for y in 0..2 {
            for x in 0..2 {
                let c = cfa.color_at(x, y);
                let v = match c {
                    CfaColor::Red => black[0],
                    CfaColor::Blue => black[3],
                    _ => {
                        // Green sharing a row with red is G1.
                        if cfa.color_at(x ^ 1, y) == CfaColor::Red { black[1] } else { black[2] }
                    }
                };
                pat[y * 2 + x] = (base_black + v) as f32;
            }
        }
        info.black_level =
            if pat.iter().all(|&v| v == pat[0]) { BlackLevel::uniform(pat[0]) } else { BlackLevel::pattern2x2(pat) };
    }
    let mut mul = mn.cam_mul;
    if wb_is_analog {
        mul = [1.0; 4];
    }
    info.white_balance = wb_from_cam_mul(mul);
    info.color_matrices = match fallback {
        Some(f) if !f.color_matrix.is_empty() => {
            let mut cm = ColorMatrices::default();
            if let Some(m) = ColorMatrix::new(
                Illuminant::D65,
                3,
                f.color_matrix.iter().take(9).map(|&v| v as f32 / 10000.0).collect(),
            ) {
                cm.color.push(m);
                cm.source = ColorSource::CameraDb;
            }
            cm
        }
        _ => db_color(&meta, cam),
    };

    // Previews: TIFF thumbnails/JpgFromRaw plus the makernote preview IFD.
    let mut previews = tiff_previews(data, 0, &tiff, info.orientation);
    for &(o, l) in &mn.previews {
        push_jpeg(data, o, l, info.orientation, &mut previews);
    }
    classify_previews(&mut previews);
    meta.previews = previews;
    if let Some(c) = mn.nef_compression {
        meta.set_extra("nef_compression", c);
    }
    if mn.color_balance_version > 0 {
        meta.set_extra("color_balance_version", mn.color_balance_version);
    }

    let nef = Arc::new(NefRaw { src, width: geo.raw_width as usize, height: raw_h as usize, payload });
    let packed_payload = nef.packed_payload();
    let random_access = match &nef.payload {
        Payload::Huffman { .. } | Payload::Ycbcr => RandomAccess::Sequential,
        _ => RandomAccess::Rows,
    };
    let units =
        vec![DecodeUnit { rect: Rect::full(geo.raw_width, raw_h), offset: offset as u64, length: bytes as u64 }];
    let n2 = nef.clone();
    let mut dec = SimpleDecoder::new(
        if model_uc.starts_with("COOLPIX ") && model_uc != "COOLPIX A" { RawFormat::Nrw } else { RawFormat::Nef },
        meta,
        info,
        Box::new(move |ctx| n2.decode(ctx)),
    );
    dec.random_access = random_access;
    dec.units = units;
    dec.packed = packed_payload;
    Ok(Box::new(dec))
}

struct Geometry {
    left: u32,
    top: u32,
    width: u32,
    height: u32,
    raw_width: u32,
}

/// The raw IFD: the largest image (by samples x bits) that is not a JPEG preview.
fn select_raw_ifd<'t, 'a>(tiff: &'t Tiff<'a>) -> Option<&'t Ifd<'a>> {
    let mut best: Option<(&Ifd, u64)> = None;
    for ifd in tiff.all_ifds() {
        let (w, h) = (ifd.width() as u64, ifd.height() as u64);
        let bps = ifd.bps() as u64;
        let spp = ifd.u32(tags::SAMPLES_PER_PIXEL).unwrap_or(1);
        if w == 0 || h == 0 || bps == 0 || bps > 32 || !ifd.has(tags::STRIP_OFFSETS) {
            continue;
        }
        if ifd.compression() == 6 && spp == 3 {
            continue;
        }
        if ifd.compression() == 6 || ifd.compression() == 7 {
            continue;
        }
        let score = w * h * bps;
        if best.is_none_or(|(_, s)| score > s) {
            best = Some((ifd, score));
        }
    }
    best.map(|(i, _)| i)
}

/// Orientation from the first IFD with a non-default orientation tag.
fn file_orientation(tiff: &Tiff) -> Orientation {
    tiff.all_ifds()
        .into_iter()
        .filter_map(|i| i.u32(tags::ORIENTATION))
        .find(|&o| (2..=8).contains(&o))
        .map(Orientation::from_exif)
        .unwrap_or(Orientation::Normal)
}

/// reference-normalised model name ("NIKON D850" → "D850", "COOLPIX P7700" → "Coolpix P7700").
fn table_model_name(model: &str) -> String {
    let m = model.trim();
    if m.len() > 8 && m[..8].eq_ignore_ascii_case("COOLPIX ") { format!("Coolpix {}", &m[8..]) } else { m.to_string() }
}

/// Camera-db mode string for the payload.
fn mode_name(p: &Payload, bps: u32, mn: &MakerNote) -> String {
    match p {
        Payload::Huffman { .. } => format!("{bps}bit-compressed"),
        Payload::Ycbcr => match mn.nef_compression {
            _ => "sNEF".into(),
        },
        Payload::Coolscan { .. } => "coolscan".into(),
        _ => format!("{bps}bit-uncompressed"),
    }
}

/// As-shot multipliers (R, G, B, G2) → normalised white balance, rejecting values the
/// reference considers implausible.
fn wb_from_cam_mul(m: [f32; 4]) -> Option<WhiteBalance> {
    if m[..3].iter().any(|&v| !(v > 0.001)) {
        return None;
    }
    let min = m[..3].iter().copied().fold(f32::MAX, f32::min);
    let norm: Vec<f32> = m[..3].iter().map(|v| v / min).collect();
    let max = norm.iter().copied().fold(0.0, f32::max);
    if max > 100.0 {
        return None;
    }
    WhiteBalance::from_multipliers(m[0], m[1], m[2], if m[3] > 0.0 { m[3] } else { m[1] })
}

impl<'a> NefRaw<'a> {
    fn packed_payload(&self) -> Option<PackedPayload<'a>> {
        match &self.payload {
            Payload::Packed(s) => s.fast_layout().filter(|l| l.order == PackOrder::MsbBytes).map(|l| PackedPayload {
                data: self.src,
                width: l.width as u32,
                height: l.height as u32,
                bits: l.bits,
                order: l.order,
                row_stride: l.row_stride,
            }),
            _ => None,
        }
    }

    fn decode(&self, ctx: &DecodeContext) -> Result<RawImage> {
        let (w, h) = (self.width, self.height);
        match &self.payload {
            Payload::HighEfficiency => {
                return Err(Error::unsupported_compression(
                    "NEF",
                    "Nikon high-efficiency (TicoRAW / JPEG XS) is a proprietary codec",
                ));
            }
            Payload::Ycbcr => return ctx.profiler.time(Stage::EntropyDecode, || self.decode_ycbcr(ctx)),
            Payload::Coolscan { samples, bits, order } => {
                return ctx.profiler.time(Stage::Unpack, || self.decode_coolscan(ctx, *samples, *bits, *order));
            }
            _ => {}
        }
        let mut img = RawImage::new_u16(w as u32, h as u32, 1, &ctx.limits)?;
        let out = img.u16_mut();
        match &self.payload {
            Payload::Huffman { meta, rows } => {
                ctx.profiler.time(Stage::EntropyDecode, || {
                    nikon_huffman::decode_nikon(self.src, meta, w, (*rows).min(h), out, w)
                })?;
            }
            Payload::Packed(spec) => {
                let spec = PackedSpec { width: w, height: h, ..*spec };
                ctx.profiler
                    .time(Stage::Unpack, || nikon_packed::decode(self.src, &spec, out, w, ctx.parallel, ctx.simd))?;
            }
            Payload::Words { order, shift } => {
                let po = if *order == ByteOrder::Little { PackOrder::LsbBytes } else { PackOrder::MsbBytes };
                let layout = PackedLayout::tight(w, h, 16, po);
                ctx.profiler
                    .time(Stage::Unpack, || packed::unpack(self.src, layout, out, w, ctx.parallel, ctx.simd, false))?;
                if *shift > 0 {
                    let s = *shift;
                    par::for_each_row_chunk(out, w, 256, ctx.parallel, |_, c| c.iter_mut().for_each(|v| *v >>= s));
                }
            }
            Payload::PaddedLsb { bits, row_stride } => {
                let layout = PackedLayout {
                    width: w,
                    height: h,
                    bits: *bits,
                    order: PackOrder::LsbBytes,
                    row_stride: *row_stride,
                };
                ctx.profiler
                    .time(Stage::Unpack, || packed::unpack(self.src, layout, out, w, ctx.parallel, ctx.simd, false))?;
            }
            Payload::HighEfficiency | Payload::Ycbcr | Payload::Coolscan { .. } => unreachable!("handled above"),
        }
        Ok(img)
    }

    /// Small/medium NEF: pairs of pixels share Cb/Cr (12-bit, little-endian packed
    /// as Y0 Y1 Cb Cr). Chroma is interpolated horizontally and converted to RGB with
    fn decode_ycbcr(&self, ctx: &DecodeContext) -> Result<RawImage> {
        let (w, h) = (self.width, self.height);
        if w < 2 {
            return Err(Error::corrupt("sNEF too narrow"));
        }
        let row_bytes = w * 3;
        if self.src.len() < row_bytes * h {
            return Err(Error::corrupt("sNEF data truncated"));
        }
        let curve = sraw_curve();
        let mut img = RawImage::new_u16(w as u32, h as u32, 3, &ctx.limits)?;
        let src = self.src;
        par::for_each_row_chunk(img.u16_mut(), w * 3, 16, ctx.parallel, |first, chunk| {
            let mut ycc = vec![[0u16; 3]; w];
            for (i, out) in chunk.chunks_mut(w * 3).enumerate() {
                let row = first + i;
                let rd = &src[row * row_bytes..(row + 1) * row_bytes];
                let mut col = 0;
                while col + 1 < w {
                    let b = &rd[col * 3..col * 3 + 6];
                    let y0 = ((b[1] as u16 & 0xf) << 8) | b[0] as u16;
                    let y1 = ((b[2] as u16) << 4) | (b[1] as u16 >> 4);
                    let cb = ((b[4] as u16 & 0xf) << 8) | b[3] as u16;
                    let cr = ((b[5] as u16) << 4) | (b[4] as u16 >> 4);
                    ycc[col] = [y0, cb, cr];
                    ycc[col + 1] = [y1, 2048, 2048];
                    col += 2;
                }
                let mut col = 0;
                while col < w {
                    let col2 = if col + 2 < w { col + 2 } else { col };
                    if col + 1 < w {
                        ycc[col + 1][1] = ((ycc[col][1] as u32 + ycc[col2][1] as u32) / 2) as u16;
                        ycc[col + 1][2] = ((ycc[col][2] as u32 + ycc[col2][2] as u32) / 2) as u16;
                    }
                    col += 2;
                }
                for (px, o) in ycc.iter().zip(out.chunks_exact_mut(3)) {
                    let [r, g, b] = ycc_to_rgb(*px);
                    o[0] = curve[(r * 3072.0) as usize];
                    o[1] = curve[(g * 3072.0) as usize];
                    o[2] = curve[(b * 3072.0) as usize];
                }
            }
        });
        Ok(img)
    }

    /// Nikon Scan NEF: interleaved RGB (or grey) samples.
    fn decode_coolscan(&self, ctx: &DecodeContext, samples: u32, bits: u32, order: ByteOrder) -> Result<RawImage> {
        let (w, h) = (self.width, self.height);
        let cpp = samples.max(1);
        let mut img = RawImage::new_u16(w as u32, h as u32, cpp, &ctx.limits)?;
        let n = w * h * cpp as usize;
        let out = img.u16_mut();
        if bits <= 8 {
            // "gamma 1.0" transfer curve: v * 65536 / 255, saturating at 255).
            let s = self.src.get(..n).ok_or_else(|| Error::corrupt("Coolscan data truncated"))?;
            let lut: Vec<u16> =
                (0..256u32).map(|i| if i < 255 { (65536.0 * (i as f64 / 255.0)) as u16 } else { 0xffff }).collect();
            for (o, &v) in out.iter_mut().zip(s) {
                *o = lut[v as usize];
            }
        } else {
            let s = self.src.get(..n * 2).ok_or_else(|| Error::corrupt("Coolscan data truncated"))?;
            for (o, c) in out.iter_mut().zip(s.chunks_exact(2)) {
                *o = order.u16([c[0], c[1]]);
            }
        }
        Ok(img)
    }
}

/// Reference YCbCr → RGB conversion of one sNEF pixel (values in 0..=1).
#[inline]
fn ycc_to_rgb(p: [u16; 3]) -> [f32; 3] {
    let mut y = p[0] as f32 / 2549.0;
    let mut ch2 = (p[1] as i32 - 1280) as f32 / 1536.0;
    let mut ch3 = (p[2] as i32 - 1280) as f32 / 1536.0;
    if y > 1.0 {
        y = 1.0;
    }
    if y > 0.803 {
        ch2 = 0.5;
        ch3 = 0.5;
    }
    let r = (y + 1.40200 * (ch3 - 0.5)).clamp(0.0, 1.0);
    let g = (y - 0.34414 * (ch2 - 0.5) - 0.71414 * (ch3 - 0.5)).clamp(0.0, 1.0);
    let b = (y + 1.77200 * (ch2 - 0.5)).clamp(0.0, 1.0);
    [r, g, b]
}

/// Tone curve applied to sNEF RGB (3073 entries, 14-bit output).
fn sraw_curve() -> Vec<u16> {
    let (b1, b2, b3, b4) =
        (-5.79342238397656E-02f64, 3.28163551282665f64, -8.43136004842678f64, 1.03533181861023E+01f64);
    (0..=3072)
        .map(|i| {
            let x = i as f64 / 3072.0;
            let y = (1.0 - (-b1 * x - b2 * x * x - b3 * x * x * x - b4 * x * x * x * x).exp()).max(0.0);
            (y * 16383.0f32 as f64) as u16
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sraw_curve_is_monotonic_and_bounded() {
        let c = sraw_curve();
        assert_eq!(c.len(), 3073);
        assert_eq!(c[0], 0);
        assert!(c.windows(2).all(|w| w[1] >= w[0]));
        assert!(*c.last().unwrap() <= 16383);
    }

    #[test]
    fn ycc_neutral_grey() {
        let [r, g, b] = ycc_to_rgb([1000, 2048, 2048]);
        assert!(r > 0.0 && (r - g).abs() < 0.2 && (b - g).abs() < 0.2);
    }

    #[test]
    fn wb_plausibility() {
        assert!(wb_from_cam_mul([0.0, 1.0, 1.0, 1.0]).is_none());
        assert!(wb_from_cam_mul([1000.0, 1.0, 1.0, 1.0]).is_none());
        let wb = wb_from_cam_mul([2.0, 1.0, 1.5, 1.0]).unwrap();
        assert_eq!(wb.coeffs[0], 2.0);
    }
}
