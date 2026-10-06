//! Nikon NEF / NRW.
//!
//! Payload variants (selected from the raw IFD's compression, size and makernote,
//! with fixed precedence rules so ambiguous files always decode the same way):
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
//! Geometry, black and white levels follow per-camera conventions (camera-specific
//! visible-area trims, makernote black in 14-bit units scaled to 12-bit files).

pub(crate) mod conventions;
mod crypt;
mod makernote;

use crate::common::*;
use conventions::{select_raw_ifd, wb_from_cam_mul};
use flashraw_codecs::nikon_huffman::{self, NikonMeta};
use flashraw_codecs::nikon_packed::{self, PackedSpec};
use flashraw_codecs::packed::{self, PackedLayout};
use flashraw_core::tiff::{Tiff, tags};
use flashraw_core::*;
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
    /// Nikon Huffman (lossy or lossless); `rows` limits decoding to the visible rows.
    Huffman { meta: Arc<NikonMeta>, rows: usize },
    /// Packed samples.
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
    /// The whole file.
    data: &'a [u8],
    /// File offset of the payload.
    offset: usize,
    /// Payload bytes from the data offset to the end of the file (some files' data
    /// runs past the declared strip length; empty when the offset is past the end).
    src: &'a [u8],
    /// Payload length the strip declares.
    declared: usize,
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
    // Model without the "NIKON " prefix.
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
    // A payload past the end of the file fails when decoded, not here (metadata and
    // previews stay available).
    let src = data.get(offset..).unwrap_or(&[]);
    let software = meta.software.clone().unwrap_or_default();
    let samples = raw.u32(tags::SAMPLES_PER_PIXEL).unwrap_or(1);

    // --- Payload selection (heuristics on compression, size and makernote) ---
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
                if w64.div_ceil(10) * 16 * h64 == b64 {
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
                    Payload::PaddedLsb { bits: 12, row_stride: (width * 3 / 2).div_ceil(16) * 16 }
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

    // --- Camera-specific geometry (visible area) ---
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
        geo.width = geo.width.saturating_sub(4);
    } else if ["D40X", "D60", "D80", "D3000"].iter().any(|m| model_matches(m)) {
        geo.height = geo.height.saturating_sub(3);
        geo.width = geo.width.saturating_sub(4);
    } else if ["D3", "D3S", "D700"].iter().any(|m| model_matches(m)) {
        geo.width = geo.width.saturating_sub(4);
        geo.left = 2;
    } else if model_matches("D3100") {
        geo.width = geo.width.saturating_sub(28);
        geo.left = 6;
    } else if model_matches("D5000") || model_matches("D90") {
        geo.width = geo.width.saturating_sub(42);
    } else if ["D5100", "D7000", "COOLPIX A"].iter().any(|m| model_matches(m)) {
        geo.width = geo.width.saturating_sub(44);
    } else if ["D3200", "D600", "D610"].iter().any(|m| model_matches(m)) || model_uc.starts_with("D800") {
        geo.width = geo.width.saturating_sub(46);
    } else if model_matches("D4") || model_matches("DF") {
        geo.width = geo.width.saturating_sub(52);
        geo.left = 2;
    } else if model_matches("D500") {
    } else if model_uc.starts_with("D40") || model_uc.starts_with("D50") || model_uc.starts_with("D70") {
        geo.width = geo.width.saturating_sub(1);
    } else if model_matches("D100") {
        if packed_flags.is_some_and(|f| f != 0) {
            geo.width = geo.width.saturating_add(3);
            geo.raw_width = geo.width + 3;
        }
    } else if model_matches("D200") {
        geo.left = 1;
        geo.width = geo.width.saturating_sub(4);
        cfa_override = Some(CfaPattern::GRBG);
    } else if model_uc.starts_with("D2H") {
        geo.left = 6;
        geo.width = geo.width.saturating_sub(14);
    } else if model_uc.starts_with("D2X") {
        geo.width = geo.width.saturating_sub(if geo.width == 3264 { 32 } else { 8 });
    } else if model_uc.starts_with("D300") {
        geo.width = geo.width.saturating_sub(32);
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
        geo.height = geo.height.saturating_sub(2);
    } else if model_uc.starts_with('E') {
        // Coolpix E-series NEF: rows stored as two interlaced fields.
        if let Payload::Packed(s) = &mut payload {
            s.flags |= 2;
        }
    }
    // Bayer visible areas are kept on even coordinates (stable CFA phase).
    if geo.left % 2 == 1 {
        geo.left = geo.left.saturating_add(1);
        geo.width = geo.width.saturating_sub(1);
    }
    if geo.top % 2 == 1 {
        geo.top = geo.top.saturating_add(1);
        geo.height = geo.height.saturating_sub(1);
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
        // Only the visible rows are decoded (trailing rows stay zero).
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
    let mode = mode_name(&payload, bps);
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
    conventions::apply_table_levels(fallback, &mut base_black, &mut black, &mut maximum);
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
    info.orientation = resolve_orientation(&tiff, &mn, &mut meta, cam);
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
        info.black_level = conventions::black_for_cfa(&cfa, base_black, black);
    }
    // White balance: the makernote's as-shot multipliers.
    let mut mul = mn.cam_mul;
    if wb_is_analog {
        mul = [1.0; 4];
    }
    info.white_balance = wb_from_cam_mul(mul);
    info.color_matrices = conventions::color_from_table(fallback, &meta, cam);

    // Previews: TIFF thumbnails/JpgFromRaw plus the makernote preview IFD.
    let mut previews = tiff_previews(data, 0, &tiff, info.orientation);
    for &(o, l) in &mn.previews {
        push_jpeg(data, o, l, info.orientation, &mut previews);
    }
    classify_previews(&mut previews);
    let (w, h) = (info.active_area.width, info.active_area.height);
    crate::orientation::resolve_previews(data, &mut previews, info.orientation, w, h);
    meta.previews = previews;
    if let Some(c) = mn.nef_compression {
        meta.set_extra("nef_compression", c);
    }
    if mn.color_balance_version > 0 {
        meta.set_extra("color_balance_version", mn.color_balance_version);
    }

    let nef = Arc::new(NefRaw {
        data,
        offset,
        src,
        declared: bytes,
        width: geo.raw_width as usize,
        height: raw_h as usize,
        payload,
    });
    let packed_payload = nef.packed_payload();
    let row_layout = nef.row_layout();
    let random_access = if row_layout.is_some() { RandomAccess::Rows } else { RandomAccess::Sequential };
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
    if let Some(rl) = row_layout {
        let n3 = nef.clone();
        dec.region = Some(Box::new(move |ctx, r| {
            conventions::decode_rows_region(n3.data, n3.offset, DataUnit::Strip(0), rl, r, ctx)
        }));
    }
    Ok(Box::new(dec))
}

/// Visible area plus the (possibly widened) buffer width.
struct Geometry {
    left: u32,
    top: u32,
    width: u32,
    height: u32,
    raw_width: u32,
}

/// Normalised model name ("NIKON D850" → "D850", "COOLPIX P7700" → "Coolpix P7700").
/// Orientation from the standard tag and the rotation saved by Nikon's software in the
/// edit record, combined under the camera's policy.
fn resolve_orientation(
    tiff: &Tiff,
    mn: &makernote::MakerNote,
    meta: &mut RawMetadata,
    cam: Option<&flashraw_camera_db::Camera>,
) -> Orientation {
    let exif = crate::orientation::tiff_orientation(tiff.all_ifds());
    let edit = mn.edit_rotation.and_then(|deg| match deg % 360 {
        0 => Some(Orientation::Normal),
        90 => Some(Orientation::Rotate90),
        180 => Some(Orientation::Rotate180),
        270 => Some(Orientation::Rotate270),
        _ => None,
    });
    let policy = crate::orientation::policy_for(meta, cam);
    crate::orientation::apply(meta, exif, edit, None, policy)
}

fn table_model_name(model: &str) -> String {
    let m = model.trim();
    if m.len() > 8 && m[..8].eq_ignore_ascii_case("COOLPIX ") { format!("Coolpix {}", &m[8..]) } else { m.to_string() }
}

/// Camera-db mode string for the payload.
fn mode_name(p: &Payload, bps: u32) -> String {
    match p {
        Payload::Huffman { .. } => format!("{bps}bit-compressed"),
        Payload::Ycbcr => "sNEF".into(),
        Payload::Coolscan { .. } => "coolscan".into(),
        _ => format!("{bps}bit-uncompressed"),
    }
}

impl<'a> NefRaw<'a> {
    /// The payload as independently addressable rows, when it is stored that way.
    fn row_layout(&self) -> Option<conventions::RowLayout> {
        let (w, h) = (self.width, self.height);
        let layout = match &self.payload {
            Payload::Packed(s) => s.fast_layout()?,
            Payload::Words { order, .. } => PackedLayout::tight(
                w,
                h,
                16,
                if *order == ByteOrder::Little { PackOrder::LsbBytes } else { PackOrder::MsbBytes },
            ),
            Payload::PaddedLsb { bits, row_stride } => {
                PackedLayout { width: w, height: h, bits: *bits, order: PackOrder::LsbBytes, row_stride: *row_stride }
            }
            _ => return None,
        };
        let shift = if let Payload::Words { shift, .. } = &self.payload { *shift } else { 0 };
        Some(conventions::RowLayout { layout, shift })
    }

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
        let unit = DataUnit::Strip(0);
        match &self.payload {
            Payload::Huffman { meta, rows } => {
                let rows = (*rows).min(h);
                let dead_row = ctx
                    .profiler
                    .time(Stage::EntropyDecode, || {
                        nikon_huffman::decode_nikon_rows(self.src, meta, w, rows, out, w, ctx.parallel, self.declared)
                    })
                    .map_err(|e| e.or_truncated(self.data.len(), unit, self.offset, self.declared))?;
                // A stream that runs dry because the strip runs past the end of the file is
                // truncation (even in the last row): recovering keeps the rows decoded from
                // real data. Otherwise only the last row may need padding bits (encoders
                // that do not flush).
                let (len, (o, d)) = (self.data.len(), (self.offset, self.declared));
                if !conventions::finish_truncated_stream(ctx, len, unit, o, d, out, w, rows, dead_row)? {
                    nikon_huffman::check_complete(dead_row, rows)?;
                }
            }
            Payload::Packed(spec) => {
                let spec = PackedSpec { width: w, height: h, ..*spec };
                let src = ctx.sensor_rows(self.data, unit, self.offset, packed_geometry(&spec))?;
                ctx.profiler.time(Stage::Unpack, || {
                    nikon_packed::decode(src, &spec, out, w, ctx.parallel, ctx.simd, ctx.recover)
                })?;
            }
            Payload::Words { order, shift } => {
                let po = if *order == ByteOrder::Little { PackOrder::LsbBytes } else { PackOrder::MsbBytes };
                let layout = PackedLayout::tight(w, h, 16, po);
                let src = ctx.sensor_rows(self.data, unit, self.offset, conventions::row_geometry(&layout, h))?;
                ctx.profiler
                    .time(Stage::Unpack, || packed::unpack(src, layout, out, w, ctx.parallel, ctx.simd, ctx.recover))?;
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
                let src = ctx.sensor_rows(self.data, unit, self.offset, conventions::row_geometry(&layout, h))?;
                ctx.profiler
                    .time(Stage::Unpack, || packed::unpack(src, layout, out, w, ctx.parallel, ctx.simd, ctx.recover))?;
            }
            Payload::HighEfficiency | Payload::Ycbcr | Payload::Coolscan { .. } => unreachable!("handled above"),
        }
        Ok(img)
    }

    /// Small/medium NEF: pairs of pixels share Cb/Cr (12-bit, little-endian packed
    /// as Y0 Y1 Cb Cr). Chroma is interpolated horizontally and converted to RGB with
    /// a fixed matrix and tone curve, giving 3 samples per pixel.
    fn decode_ycbcr(&self, ctx: &DecodeContext) -> Result<RawImage> {
        let (w, h) = (self.width, self.height);
        if w < 2 {
            return Err(Error::corrupt("sNEF too narrow"));
        }
        let row_bytes = w * 3;
        // Pixel pairs are 6 bytes: a row is only usable whole.
        let geometry = RowGeometry { rows: h, stride: row_bytes, row_bytes, samples: w * 3 };
        let src = ctx.sensor_rows(self.data, DataUnit::Strip(0), self.offset, geometry)?;
        let curve = sraw_curve();
        let mut img = RawImage::new_u16(w as u32, h as u32, 3, &ctx.limits)?;
        par::for_each_row_chunk_bounded(
            img.u16_mut(),
            w * 3,
            par::memory_bound_workers(),
            ctx.parallel,
            |first, chunk| {
                let mut ycc = vec![[0u16; 3]; w];
                for (i, out) in chunk.chunks_mut(w * 3).enumerate() {
                    let row = first + i;
                    // Rows missing from truncated data (recovering) stay zero.
                    let Some(rd) = src.get(row * row_bytes..(row + 1) * row_bytes) else { break };
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
            },
        );
        Ok(img)
    }

    /// Nikon Scan NEF: interleaved RGB (or grey) samples.
    fn decode_coolscan(&self, ctx: &DecodeContext, samples: u32, bits: u32, order: ByteOrder) -> Result<RawImage> {
        let (w, h) = (self.width, self.height);
        let cpp = samples.max(1);
        let mut img = RawImage::new_u16(w as u32, h as u32, cpp, &ctx.limits)?;
        let n = w * h * cpp as usize;
        let out = img.u16_mut();
        let bits = if bits <= 8 { 8 } else { 16 };
        let geometry = RowGeometry::tight(h, w * cpp as usize, bits);
        // Missing samples of truncated data (recovering) stay zero.
        let s = ctx.sensor_rows(self.data, DataUnit::Strip(0), self.offset, geometry)?;
        if bits == 8 {
            // 8-bit scans are expanded to the 16-bit range (a linear transfer
            // curve: v * 65536 / 255, saturating at 255).
            let s = &s[..n.min(s.len())];
            let lut: Vec<u16> =
                (0..256u32).map(|i| if i < 255 { (65536.0 * (i as f64 / 255.0)) as u16 } else { 0xffff }).collect();
            for (o, &v) in out.iter_mut().zip(s) {
                *o = lut[v as usize];
            }
        } else {
            for (o, c) in out.iter_mut().zip(s.chunks_exact(2)) {
                *o = order.u16([c[0], c[1]]);
            }
        }
        Ok(img)
    }
}

/// Byte geometry of a load-flags packed payload: full rows (the exact layouts gather
/// bits in multi-byte units, so the last row is needed whole).
pub(crate) fn packed_geometry(spec: &PackedSpec) -> RowGeometry {
    match spec.fast_layout() {
        Some(l) => conventions::row_geometry(&l, spec.height),
        None => {
            let rb = spec.row_bytes();
            let rows = if rb == 0 { 0 } else { spec.required_bytes().div_ceil(rb) };
            RowGeometry { rows, stride: rb, row_bytes: rb, samples: spec.width * spec.height / rows.max(1) }
        }
    }
}

/// YCbCr → RGB conversion of one sNEF pixel (values in 0..=1).
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

    /// One corpus sample per payload variant: the original must decode, and truncated
    /// or corrupted copies must fail cleanly (or decode), never panic.
    #[test]
    fn malformed_inputs_do_not_panic() {
        use conventions::robustness::{corpus_file, mutate, open_decode};
        let samples = [
            "Nikon/D1/RAW_NIKON_D1.NEF",
            "Nikon/D100/DSC_2376.NEF",
            "Nikon/E5400/RAW_NIKON_E5400.NEF",
            "Nikon/D40/DSC_1842.NEF",
            "Nikon/D50/DSC_5155.NEF",
            "Nikon/D3/JD1_8205.NEF",
            "Nikon/D3/JD1_8207.NEF",
            "Nikon/D4S/FU9_3802.NEF",
            "Nikon/D4S/FU9_3807.NEF",
            "Nikon/Coolpix P7000/RAW_NIKON_P7000.NRW",
            "Nikon/Coolpix P330/DSCN0138.NRW",
            "Nikon/Z 6/DSC_0753.NEF",
            "Nikon/Nikon COOLSCAN V ED/Image21-600pixels.nef",
        ];
        let mut tested = 0;
        for (i, s) in samples.iter().enumerate() {
            let Some(data) = corpus_file(s) else { continue };
            open_decode(&data).unwrap_or_else(|e| panic!("{s}: {e}"));
            mutate(&data, i as u64 * 7919 + 1);
            tested += 1;
        }
        // High-efficiency files are recognised but rejected with a clear error.
        if let Some(d) = corpus_file("NIKON CORPORATION/NIKON Z50_2/DSC_0062_DX_HE.NEF") {
            let e = open_decode(&d).unwrap_err();
            assert_eq!(e.kind(), "UnsupportedCompression");
        }
        eprintln!("nikon robustness: {tested} samples");
    }

    /// A Huffman NEF cut by the end of the file: rows decoded from real data are kept and the
    /// rest zero-filled with a warning under the recover policy (even when only the last
    /// byte is missing); a structured truncation under strict.
    #[test]
    fn truncated_huffman_stream_recovers() {
        use conventions::robustness::{corpus_file, truncated_stream_recovers};
        for s in ["Nikon/D3/JD1_8205.NEF", "Nikon/Z fc/_DSC0018.nef"] {
            let Some(data) = corpus_file(s) else { continue };
            truncated_stream_recovers(s, &data);
        }
    }

    #[test]
    fn synthetic_nef_without_payload_is_rejected() {
        // A NIKON TIFF whose raw IFD points past the end of the file.
        let mut d = b"MM\0*\0\0\0\x08".to_vec();
        let entries: [(u16, u16, u32, u32); 7] = [
            (0x100, 4, 1, 4000),
            (0x101, 4, 1, 3000),
            (0x102, 3, 1, 12 << 16),
            (0x103, 3, 1, 34713 << 16),
            (0x10f, 2, 4, u32::from_be_bytes(*b"NIK\0")),
            (0x111, 4, 1, 1_000_000),
            (0x117, 4, 1, 100),
        ];
        d.extend((entries.len() as u16).to_be_bytes());
        for (tag, typ, cnt, val) in entries {
            d.extend(tag.to_be_bytes());
            d.extend(typ.to_be_bytes());
            d.extend(cnt.to_be_bytes());
            d.extend(val.to_be_bytes());
        }
        d.extend([0u8; 4]);
        let tiff = Tiff::parse(&d).unwrap();
        assert!(open_tiff(&d, tiff, &OpenOptions::default()).is_err());
    }

    /// Region decodes of row-addressable payloads equal crops of full decodes.
    #[test]
    fn region_decode_matches_full_decode() {
        use conventions::robustness::{corpus_file, region_matches_full};
        for s in [
            "Nikon/D1/RAW_NIKON_D1.NEF",
            "Nikon/D3/JD1_8207.NEF",
            "Nikon/D4S/FU9_3807.NEF",
            "Nikon/Z 6/DSC_0753.NEF",
            "Nikon/Z 6/DSC_0750.NEF",
            "Nikon/Coolpix P7000/RAW_NIKON_P7000.NRW",
            "Nikon/Coolpix P330/DSCN0138.NRW",
        ] {
            let Some(data) = corpus_file(s) else { continue };
            assert!(region_matches_full(&data), "{s} should be row addressable");
        }
    }
}
