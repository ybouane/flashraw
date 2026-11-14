//! Nikon NEF / NRW reader (container, metadata, payload selection).
//!
//! A Nikon raw file is a TIFF. IFD0 carries the camera, the EXIF IFD carries the maker
//! note ([`makernote`]), and the raw image is the largest non-preview IFD. How its strip is
//! coded is not stated by one tag: TIFF compression 34713 covers Huffman-coded, 12-bit
//! packed, 16-bit word and sNEF (YCbCr) payloads, which are told apart by the strip size
//! (see [`choose_payload`]). The visible area, black and white levels and CFA phase of the
//! sensor buffer are per-camera facts kept in the [`MODEL_RULES`] table.

pub(crate) mod conventions;
pub(crate) mod crypt;
pub(crate) mod makernote;

use crate::common::*;
use crate::orientation;
use flashraw_codecs::nikon_huffman::{self, NikonMeta};
use flashraw_codecs::nikon_packed::{self, PackedSpec};
use flashraw_codecs::packed::{self, PackedLayout};
use flashraw_core::tiff::{Tiff, tags};
use flashraw_core::*;
use std::sync::{Arc, OnceLock};

/// Magic-signature probe (NEF/NRW are TIFF based and dispatched by make).
pub fn probe(_data: &[u8]) -> bool {
    false
}

/// Not a stand-alone format: use [`open_tiff`].
pub fn open<'a>(_data: &'a [u8], _opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    Err(Error::UnsupportedFormat("NEF files are TIFF-based; use `open_tiff`".into()))
}

/// Byte geometry of a load-flags packed payload (used by the Pentax and Samsung readers too).
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

// ---------------------------------------------------------------------------------------------
// Payloads
// ---------------------------------------------------------------------------------------------

/// How the strip of the raw IFD is coded.
#[derive(Debug, Clone)]
enum Payload {
    /// Film-scanner image: `samples` per pixel, 8 or 16 bits.
    Coolscan { samples: u32 },
    /// Bit-packed samples described by load flags.
    Packed(PackedSpec),
    /// 16-bit words in `order`, each shifted right by `shift`.
    Words { order: ByteOrder, shift: u32 },
    /// LSB-first packed rows padded to `row_stride` bytes.
    PaddedLsb { bits: u32, row_stride: usize },
    /// YCbCr 4:2:2, 12 bits (the D4S small NEF).
    SNef,
    /// JPEG XS based "high efficiency": not decodable.
    HighEfficiency,
    /// Nikon Huffman with a linearisation curve from the maker note.
    Huffman,
}

/// Facts about the raw IFD the payload selection looks at.
struct RawFacts<'a> {
    width: u64,
    height: u64,
    /// Declared bytes of the first strip.
    bytes: u64,
    bps: u32,
    compression: u32,
    photometric: Option<u32>,
    samples: u32,
    scanner: bool,
    file_order: ByteOrder,
    /// IFD0 `Model` as stored (with the make prefix).
    raw_model: &'a str,
    /// Upper-case model without the make prefix.
    model: &'a str,
    /// The file from the first strip on.
    strip: &'a [u8],
}

/// TIFF photometric interpretation of colour-filter-array data.
const PHOTOMETRIC_CFA: u32 = 32803;
/// TIFF photometric interpretation RGB.
const PHOTOMETRIC_RGB: u32 = 2;
/// The compression code Nikon uses for compressed and several uncompressed payloads.
const COMPRESSION_NIKON: u32 = 34713;
/// Start of a JPEG XS codestream (SOC, CAP).
const JPEG_XS_START: [u8; 4] = [0xFF, 0x10, 0xFF, 0x50];
/// Load flags of `nikon_packed`: a filler byte after every ten samples.
const FLAGS_FILLER: u32 = 1;
/// Two interlaced fields, and the second field's own offset.
const FLAG_FIELDS: u32 = 2;
const FLAGS_RGB_FIELDS: u32 = 6;
/// Load flags of the later Coolpix NRW files.
const FLAGS_NRW: u32 = 24;
/// Load flags of the Z fc uncompressed 12-bit file.
const FLAGS_TIGHT_N: u32 = 80;

/// `ceil(a / b)` in 128 bits (the strip relations multiply two 32-bit dimensions).
fn ceil_div(a: u128, b: u128) -> u128 {
    a.div_ceil(b)
}

/// Row stride of rows of `width` samples of `bits` bits padded to 16 bytes.
fn padded_stride(width: u64, bits_num: u128, bits_den: u128) -> u128 {
    16 * ceil_div(width as u128 * bits_num / bits_den, 16)
}

fn packed(bits: u32, flags: u32, f: &RawFacts) -> Payload {
    Payload::Packed(PackedSpec { width: f.width as usize, height: f.height as usize, bits, flags, field2_offset: None })
}

/// Which coding the strip uses. The first relation between strip size and dimensions that
/// holds decides; the order matters only where two hold at once.
fn choose_payload(f: &RawFacts) -> Result<Payload> {
    let (w, h, b) = (f.width as u128, f.height as u128, f.bytes as u128);
    let coolscan = f.scanner || (f.bps == 8 && f.compression == 1 && f.photometric != Some(PHOTOMETRIC_CFA));
    let payload = if coolscan {
        Payload::Coolscan { samples: f.samples }
    } else {
        match f.compression {
            COMPRESSION_NIKON => {
                if ceil_div(w, 10) * 16 * h == b {
                    packed(12, FLAGS_FILLER, f)
                } else if w * h * 3 == 2 * b {
                    packed(f.bps, if f.raw_model.starts_with('N') { FLAGS_TIGHT_N } else { 0 }, f)
                } else if w * h * 3 == b {
                    Payload::SNef
                } else if w * h * 2 == b {
                    Payload::Words { order: ByteOrder::Big, shift: 4 }
                } else if padded_stride(f.width, 3, 2) * h == b {
                    Payload::PaddedLsb { bits: 12, row_stride: padded_stride(f.width, 3, 2) as usize }
                } else if f.strip.starts_with(&JPEG_XS_START) {
                    Payload::HighEfficiency
                } else {
                    Payload::Huffman
                }
            }
            1 | 0 => match f.bps {
                8 => return Err(Error::unsupported_compression("NEF", "8-bit uncompressed")),
                12 if f.model.starts_with("COOLPIX A1000") && b == w * h * 2 => {
                    Payload::Words { order: f.file_order, shift: 0 }
                }
                12 => packed(12, if f.photometric == Some(PHOTOMETRIC_RGB) { FLAGS_RGB_FIELDS } else { 0 }, f),
                14 | 16 => Payload::Words { order: f.file_order, shift: 0 },
                bps => return Err(Error::unsupported_compression("NEF", format!("uncompressed {bps}-bit"))),
            },
            c => return Err(Error::unsupported_compression("NEF", format!("TIFF compression {c}"))),
        }
    };
    if f.bps == 8 && !coolscan {
        return Err(Error::unsupported_compression("NEF", "8-bit uncompressed"));
    }
    // Z cameras and the D6 write uncompressed 14-bit rows padded to 16 bytes.
    let wide_rows = padded_stride(f.width, 7, 4) * h == b;
    if (f.model.starts_with('Z') || f.model == "D6") && wide_rows {
        return Ok(Payload::PaddedLsb { bits: 14, row_stride: padded_stride(f.width, 7, 4) as usize });
    }
    Ok(payload)
}

// ---------------------------------------------------------------------------------------------
// Per-model rules
// ---------------------------------------------------------------------------------------------

/// How sure we are of a rule.
///
/// * `Verified`: a corpus sample reproduces it and the decoded edge columns / rows agree.
/// * `Partial`: verified for some of what the rule does (stated on the rule).
/// * `Unverified`: kept from earlier behaviour; no sample file exists.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
enum Status {
    Verified,
    Partial,
    Unverified,
}

/// Which files a rule applies to; the first rule of [`MODEL_RULES`] that matches wins.
enum Select {
    /// Upper-case model without make prefix equals one of `exact` or starts with one of `prefix`.
    Names { exact: &'static [&'static str], prefix: &'static [&'static str] },
    /// The raw IFD is this many pixels wide (reached only by models no earlier rule names).
    RawWidth(u32),
}

/// What a rule does to the buffer description.
enum Effect {
    /// The camera's white balance tag is analogue: use unit multipliers.
    UnitWhiteBalance,
    /// The visible area starts at column `left` and loses `narrow` columns and `shorten`
    /// bottom rows (garbage columns / rows the sensor does not write).
    Trim { left: u32, narrow: u32, shorten: u32 },
    /// As `Trim`, and the CFA phase of the visible area is not the file's.
    TrimWithCfa { left: u32, narrow: u32, cfa: CfaPattern },
    /// D100: a filler-byte layout stores three more columns than the TIFF says and pads the
    /// buffer by three again.
    D100Padding,
    /// D2X: 4320-wide files lose 8 columns, the 3264-wide mode loses 32.
    D2xBorder,
    /// 4032-wide compacts: P7700 / P7800 saturate at 65504 and use unshifted words, P340
    /// uses unshifted words / flags 0. No later rule applies to these files.
    Compact4032,
    /// Coolpix NRW files: load flags 24; P models have an RGGB CFA at the buffer origin
    /// and P7x00 a fixed black level at high ISO.
    CoolpixNrw,
    /// E-series Coolpix: interlaced fields.
    InterlacedFields,
}

struct ModelRule {
    select: Select,
    effect: Effect,
}

const fn names(exact: &'static [&'static str], prefix: &'static [&'static str]) -> Select {
    Select::Names { exact, prefix }
}

const fn trim(left: u32, narrow: u32, shorten: u32) -> Effect {
    Effect::Trim { left, narrow, shorten }
}

/// R1: D1. Verified (D1 sample).
const R_D1: ModelRule = ModelRule { select: names(&["D1"], &[]), effect: Effect::UnitWhiteBalance };
/// R2: D1X. Verified (right 4 columns are dark).
const R_D1X: ModelRule = ModelRule { select: names(&["D1X"], &[]), effect: trim(0, 4, 0) };
/// R3: D40X, D60, D80, D3000: 4 dark columns and 3 unwritten rows. Verified.
const R_D40X_D60_D80_D3000: ModelRule =
    ModelRule { select: names(&["D40X", "D60", "D80", "D3000"], &[]), effect: trim(0, 4, 3) };
/// R4: D3, D3S, D700: 2 dark columns on each side. Verified.
const R_D3_D3S_D700: ModelRule = ModelRule { select: names(&["D3", "D3S", "D700"], &[]), effect: trim(2, 4, 0) };
/// R5: D3100. Verified.
const R_D3100: ModelRule = ModelRule { select: names(&["D3100"], &[]), effect: trim(6, 28, 0) };
/// R6: D5000, D90. Verified.
const R_D5000_D90: ModelRule = ModelRule { select: names(&["D5000", "D90"], &[]), effect: trim(0, 42, 0) };
/// R7: D5100, D7000, Coolpix A. Verified.
const R_D5100_D7000_A: ModelRule =
    ModelRule { select: names(&["D5100", "D7000", "COOLPIX A"], &[]), effect: trim(0, 44, 0) };
/// R8: D3200, D600, D610, D800 and variants. Verified (no D800E sample).
const R_D3200_D600_D610_D800: ModelRule =
    ModelRule { select: names(&["D3200", "D600", "D610"], &["D800"]), effect: trim(0, 46, 0) };
/// R9: D4, Df. Verified.
const R_D4_DF: ModelRule = ModelRule { select: names(&["D4", "DF"], &[]), effect: trim(2, 52, 0) };
/// R10: D500, no trim; the rule exists to keep it out of the D50* prefix rule. Verified.
const R_D500: ModelRule = ModelRule { select: names(&["D500"], &[]), effect: trim(0, 0, 0) };
/// R11: D40*, D50*, D70*: the last column is dark. Verified.
const R_D40_D50_D70: ModelRule = ModelRule { select: names(&[], &["D40", "D50", "D70"]), effect: trim(0, 1, 0) };
/// R12: D100. Partial: verified for the filler-byte layout (flags 1); the other packed
/// layouts of the rule are unverified.
const R_D100: ModelRule = ModelRule { select: names(&["D100"], &[]), effect: Effect::D100Padding };
/// R13: D200: first column dark, last three flat, and the CFA starts on green. Verified.
const R_D200: ModelRule = ModelRule {
    select: names(&["D200"], &[]),
    effect: Effect::TrimWithCfa { left: 1, narrow: 4, cfa: CfaPattern::GRBG },
};
/// R14: D2H, D2Hs. Verified.
const R_D2H: ModelRule = ModelRule { select: names(&[], &["D2H"]), effect: trim(6, 14, 0) };
/// R15: D2X, D2Xs. Partial: the 4320-wide case is verified, the 3264-wide case has no sample.
const R_D2X: ModelRule = ModelRule { select: names(&[], &["D2X"]), effect: Effect::D2xBorder };
/// R16: D300, D300S. Verified.
const R_D300: ModelRule = ModelRule { select: names(&[], &["D300"]), effect: trim(0, 32, 0) };
/// R17: any other camera with 4032-wide data. Verified (P7700, P7800, P340, P330).
const R_COMPACT_4032: ModelRule = ModelRule { select: Select::RawWidth(4032), effect: Effect::Compact4032 };
/// R18: Coolpix P and B700 NRW. Partial: flags 24 and the RGGB phase are pinned by seven
/// regression files. No black override: the P7000 (ISO 800, V1.2) and P7100 (ISO 3200, V1.0)
/// samples have 372146 and 1413795 image pixels at 0, so their data is already black-subtracted;
/// the maker note or the camera table applies.
const R_COOLPIX_NRW: ModelRule =
    ModelRule { select: names(&[], &["COOLPIX P", "COOLPIX B700"]), effect: Effect::CoolpixNrw };
/// R19: Nikon 1: the last two rows are never written. Verified.
const R_NIKON_1: ModelRule = ModelRule { select: names(&[], &["1 "]), effect: trim(0, 0, 2) };
/// R20: Coolpix E series. Verified (four E files are pinned).
const R_E_SERIES: ModelRule = ModelRule { select: names(&[], &["E"]), effect: Effect::InterlacedFields };

/// In order of precedence: exact names before the prefixes that would swallow them
/// (D3000, D5000, D7000, D700, D40X, D500).
static MODEL_RULES: [ModelRule; 20] = [
    R_D1,
    R_D1X,
    R_D40X_D60_D80_D3000,
    R_D3_D3S_D700,
    R_D3100,
    R_D5000_D90,
    R_D5100_D7000_A,
    R_D3200_D600_D610_D800,
    R_D4_DF,
    R_D500,
    R_D40_D50_D70,
    R_D100,
    R_D200,
    R_D2H,
    R_D2X,
    R_D300,
    R_COMPACT_4032,
    R_COOLPIX_NRW,
    R_NIKON_1,
    R_E_SERIES,
];

/// The visible rectangle of the sensor buffer.
struct Visible {
    left: u32,
    width: u32,
    height: u32,
    /// Row length of the buffer (differs from the TIFF width for the D100 filler layout).
    buffer_width: u32,
}

/// What the rules decided besides the rectangle.
#[derive(Default)]
struct Quirks {
    white_level: Option<u32>,
    cfa: Option<CfaPattern>,
    unit_white_balance: bool,
}

/// What a rule may look at.
struct RuleInput<'a> {
    /// Upper-case model without make prefix.
    model: &'a str,
    raw_width: u32,
}

impl Select {
    fn matches(&self, input: &RuleInput) -> bool {
        match self {
            Select::Names { exact, prefix } => {
                exact.contains(&input.model) || prefix.iter().any(|p| input.model.starts_with(p))
            }
            Select::RawWidth(w) => input.raw_width == *w,
        }
    }
}

impl Effect {
    fn apply(&self, input: &RuleInput, payload: &mut Payload, vis: &mut Visible, q: &mut Quirks) {
        match self {
            Effect::UnitWhiteBalance => q.unit_white_balance = true,
            Effect::Trim { left, narrow, shorten } => {
                vis.left = *left;
                vis.width = vis.width.saturating_sub(*narrow);
                vis.height = vis.height.saturating_sub(*shorten);
            }
            Effect::TrimWithCfa { left, narrow, cfa } => {
                vis.left = *left;
                vis.width = vis.width.saturating_sub(*narrow);
                q.cfa = Some(*cfa);
            }
            Effect::D100Padding => {
                if matches!(payload, Payload::Packed(s) if s.flags != 0) {
                    vis.width = vis.width.saturating_add(3);
                    vis.buffer_width = vis.width.saturating_add(3);
                }
            }
            Effect::D2xBorder => vis.width = vis.width.saturating_sub(if input.raw_width == 3264 { 32 } else { 8 }),
            Effect::Compact4032 => match input.model {
                "COOLPIX P7700" | "COOLPIX P7800" => {
                    // 12-bit codes stored << 4 (values step by 16); the P7700 sample clips at 65504
                    // (4094 << 4, 116 image pixels), the same top code the P340 and P7100 show.
                    q.white_level = Some(65504);
                    set_word_shift(payload, 0);
                }
                "COOLPIX P340" => {
                    set_word_shift(payload, 0);
                    if let Payload::Packed(s) = payload {
                        s.flags = 0;
                    }
                }
                _ => {}
            },
            Effect::CoolpixNrw => {
                if let Payload::Packed(s) = payload {
                    s.flags = FLAGS_NRW;
                }
                if input.model.starts_with("COOLPIX P") {
                    q.cfa = Some(CfaPattern::RGGB);
                }
            }
            Effect::InterlacedFields => {
                if let Payload::Packed(s) = payload {
                    s.flags |= FLAG_FIELDS;
                }
            }
        }
    }
}

fn set_word_shift(payload: &mut Payload, new_shift: u32) {
    if let Payload::Words { shift, .. } = payload {
        *shift = new_shift;
    }
}

/// Apply the first matching rule.
fn apply_model_rules(input: &RuleInput, payload: &mut Payload, vis: &mut Visible, q: &mut Quirks) {
    if let Some(rule) = MODEL_RULES.iter().find(|r| r.select.matches(input)) {
        rule.effect.apply(input, payload, vis, q);
    }
    // Bayer phase: keep whole CFA cells (top is always 0 here).
    if vis.left % 2 == 1 {
        vis.left += 1;
        vis.width = vis.width.saturating_sub(1);
    }
}

// ---------------------------------------------------------------------------------------------
// Naming helpers
// ---------------------------------------------------------------------------------------------

/// The camera database / colour table spelling: "COOLPIX P7000" is "Coolpix P7000".
fn table_name(model: &str) -> String {
    match model.get(..8) {
        Some(head) if head.eq_ignore_ascii_case("COOLPIX ") && model.len() > 8 => {
            format!("Coolpix {}", &model[8..]).trim().to_string()
        }
        _ => model.trim().to_string(),
    }
}

/// White balance of a four-colour sensor (E5700): the camera's red / blue levels turned
/// into four multipliers through the colour matrix.
fn four_colour_wb(colour: &ColorMatrices, rb_levels: Option<[f32; 2]>) -> Option<WhiteBalance> {
    let [r, b] = rb_levels?;
    let plausible = |v: f32| v.is_finite() && (0.05..=20.0).contains(&v);
    if !(plausible(r) && plausible(b)) {
        return None;
    }
    let m = colour.preferred().filter(|m| m.rows == 4)?;
    let mul = m.multipliers_for([1.0 / r, 1.0, 1.0 / b])?;
    WhiteBalance::from_multipliers(mul[0], mul[1], mul[2], mul[3])
}

fn rotation_from_degrees(deg: u16) -> Option<Orientation> {
    match deg % 360 {
        0 => Some(Orientation::Normal),
        90 => Some(Orientation::Rotate90),
        180 => Some(Orientation::Rotate180),
        270 => Some(Orientation::Rotate270),
        _ => None,
    }
}

// ---------------------------------------------------------------------------------------------
// Opening
// ---------------------------------------------------------------------------------------------

/// Everything the decode closure needs.
struct NefRaw<'a> {
    data: &'a [u8],
    offset: usize,
    /// The file from the strip on (the stream may run past the declared length).
    src: &'a [u8],
    declared: usize,
    /// Row length of the output buffer.
    width: usize,
    /// Rows of the output buffer.
    height: usize,
    /// Rows that hold image data (the Huffman decoder stops there).
    visible_rows: usize,
    payload: Payload,
    huffman: Option<NikonMeta>,
    /// Bits per sample of the TIFF, samples per pixel of scanner files.
    bps: u32,
    file_order: ByteOrder,
}

/// Open a NEF / NRW / Nikon Scan NEF.
pub fn open_tiff<'a>(data: &'a [u8], tiff: Tiff<'a>, opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    // --- Names and maker note ---
    let ifd0 = tiff.ifds.first().ok_or_else(|| Error::corrupt("NEF without IFD0"))?;
    let mut meta = RawMetadata::default();
    flashraw_core::exif::read_ifd0(ifd0, &mut meta);
    let any_string = |tag| tiff.all_ifds().into_iter().filter_map(|i| i.string(tag)).find(|s| !s.is_empty());
    if meta.make.is_empty() {
        meta.make = any_string(tags::MAKE).unwrap_or_default();
    }
    if meta.model.is_empty() {
        meta.model = any_string(tags::MODEL).unwrap_or_default();
    }
    let model = strip_make_prefix(&meta.model, "NIKON");
    let model_uc = model.to_ascii_uppercase();
    let note =
        tiff.find_entry(tags::MAKER_NOTE).map(|e| makernote::parse(data, e, tiff.order, &model)).unwrap_or_default();
    if meta.exposure.iso.is_none() {
        meta.exposure.iso = note.iso;
    }
    if meta.serial.is_none() {
        meta.serial = note.serial.clone();
    }
    if meta.lens.focal_range.is_none() {
        if let Some([min, max, ap_min, ap_max]) = note.lens_spec {
            meta.lens.focal_range = Some((min, max));
            if ap_min > 0.0 {
                meta.lens.aperture_range = Some((ap_min, ap_max));
            }
        }
    }
    let software = meta.software.clone().unwrap_or_default();
    let scanner = software.starts_with("Nikon Scan") || model_uc.contains("COOLSCAN");

    // --- Raw IFD and strip ---
    let raw = conventions::select_raw_ifd(&tiff, scanner)
        .ok_or_else(|| no_raw_ifd(&tiff, "NEF", Error::invalid_metadata("NEF has no raw image IFD")))?;
    let (raw_w, raw_h) = (raw.width(), raw.height());
    opts.limits.check_dims(raw_w, raw_h, 1)?;
    let bps = raw.bps();
    let (offset, bytes) = *strips(raw).first().ok_or_else(|| Error::invalid_metadata("NEF raw IFD without strips"))?;
    // A payload past the end of the file fails when decoded (metadata and previews stay available).
    let src = data.get(offset..).unwrap_or(&[]);

    let facts = RawFacts {
        width: raw_w as u64,
        height: raw_h as u64,
        bytes: bytes as u64,
        bps,
        compression: raw.compression(),
        photometric: raw.u32(tags::PHOTOMETRIC),
        samples: raw.u32(tags::SAMPLES_PER_PIXEL).unwrap_or(1),
        scanner,
        file_order: tiff.order,
        raw_model: &meta.model,
        model: &model_uc,
        strip: src,
    };
    let mut payload = choose_payload(&facts)?;

    // --- Visible area and per-model conventions ---
    let mut vis = Visible { left: 0, width: raw_w, height: raw_h, buffer_width: raw_w };
    let mut quirks = Quirks::default();
    let input = RuleInput { model: &model_uc, raw_width: raw_w };
    apply_model_rules(&input, &mut payload, &mut vis, &mut quirks);
    let buffer_width = vis.buffer_width;
    if let Payload::Packed(s) = &mut payload {
        s.width = buffer_width as usize;
        s.height = raw_h as usize;
    }

    // --- The Huffman metadata ---
    let order = note.order.unwrap_or(tiff.order);
    let huffman = if matches!(payload, Payload::Huffman) {
        let lin =
            note.lin_offset.ok_or_else(|| Error::invalid_metadata("compressed NEF without linearization table"))?;
        Some(NikonMeta::parse(data.get(lin..).unwrap_or(&[]), order, bps)?)
    } else {
        None
    };
    let visible_rows = vis.height.min(raw_h) as usize;

    // --- Levels and bit depth ---
    let snef = matches!(payload, Payload::SNef);
    let coolscan = matches!(payload, Payload::Coolscan { .. });
    let bits_out = if snef {
        14
    } else if coolscan {
        16
    } else {
        bps.min(16)
    };
    let mode = match &payload {
        Payload::Huffman => format!("{bps}bit-huffman"),
        Payload::SNef => "small-ycbcr".to_string(),
        Payload::Coolscan { .. } => "film-scan".to_string(),
        _ => format!("{bps}bit-plain"),
    };
    let named = table_name(&model);
    meta.canonical_make = "Nikon".to_string();
    meta.canonical_model = named.clone();
    let cam = db_lookup(&mut meta, &mode);
    let table = flashraw_camera_db::find_matrix("Nikon", &named);

    let (mut base_black, mut black) = (0u32, [0u32; 4]);
    if let Some(b) = note.black {
        let m = b.into_iter().min().unwrap_or(0);
        base_black = u32::from(m);
        black = b.map(|v| u32::from(v - m));
    }
    if let Some(nb) = note.nrw_black {
        base_black = nb;
    }
    let mut maximum = quirks.white_level.unwrap_or(0);
    conventions::apply_table_levels(table, &mut base_black, &mut black, &mut maximum);
    if maximum == 0 {
        maximum = (1u32 << bps.clamp(1, 16)) - 1;
    }
    let twelve_bit_payload = matches!(payload, Payload::Huffman | Payload::Packed(_))
        || matches!(payload, Payload::PaddedLsb { bits: 12, .. });
    if twelve_bit_payload && bps == 12 && !model_uc.starts_with("COOLPIX") {
        // The maker note's black level is in 14-bit units.
        maximum = 4095;
        base_black /= 4;
        black = black.map(|v| v / 4);
    }
    if snef {
        maximum = 16383;
        base_black = 0;
        black = [0; 4];
    } else if coolscan {
        maximum = 65535;
        base_black = 0;
        black = [0; 4];
    }

    // --- Frame description ---
    let mut info = FrameInfo::new(buffer_width, raw_h, bits_out as u8);
    info.cpp = match &payload {
        Payload::SNef => 3,
        Payload::Coolscan { samples } => *samples,
        _ => 1,
    };
    info.compression = match payload {
        Payload::Huffman => Compression::NikonHuffman,
        Payload::HighEfficiency => Compression::NikonHighEfficiency,
        _ => Compression::Uncompressed,
    };
    let active = Rect::new(vis.left, 0, vis.width, vis.height);
    info.active_area = active;
    info.crop = active;
    if let Some([x, y, w, h]) = note.crop_area {
        let (x, y, w, h) = (active.x as u64 + x as u64, active.y as u64 + y as u64, w as u64, h as u64);
        let inside = w > 0
            && h > 0
            && x + w <= active.x as u64 + active.width as u64
            && y + h <= active.y as u64 + active.height as u64;
        if inside {
            info.crop = even_crop_origin(Rect::new(x as u32, y as u32, w as u32, h as u32), active);
        }
    }
    info.white_level = WhiteLevel::uniform(maximum);

    // Orientation: the TIFF tag and the Nikon Capture edit record.
    let exif_orientation = orientation::tiff_orientation(tiff.all_ifds());
    let edit_orientation = note.edit_rotation.and_then(rotation_from_degrees);
    let policy = orientation::policy_for(&meta, cam);
    info.orientation = orientation::apply(&mut meta, exif_orientation, edit_orientation, None, policy);

    if snef || coolscan {
        info.layout = if snef || matches!(payload, Payload::Coolscan { samples } if samples >= 3) {
            SensorLayout::Linear
        } else {
            SensorLayout::Monochrome
        };
        info.cfa = None;
    } else {
        let packed_database_cfa = matches!(&payload, Payload::Packed(s) if s.flags & FLAG_FIELDS != 0);
        let database_cfa = cam.and_then(|c| c.cfa).and_then(parse_db_cfa);
        let cfa = quirks
            .cfa
            .or(if packed_database_cfa { database_cfa } else { None })
            .or_else(|| tiff_ep_cfa(raw))
            .or(database_cfa)
            .unwrap_or(CfaPattern::RGGB);
        info.cfa = Some(cfa);
        info.layout = layout_for(&cfa);
        info.black_level = conventions::black_for_cfa(&cfa, base_black, black);
    }
    let multipliers = if quirks.unit_white_balance { [1.0; 4] } else { note.cam_mul };
    info.white_balance = conventions::wb_from_cam_mul(multipliers);
    info.white_balance_applied = snef;
    info.color_matrices = conventions::color_from_table(table, &meta, cam);
    if info.cfa.is_some_and(|c| c.four_colour_order().is_some()) {
        info.white_balance = four_colour_wb(&info.color_matrices, note.rb_levels);
    }

    // --- Previews and the remaining metadata ---
    let mut previews = tiff_previews(data, 0, &tiff, info.orientation);
    for &(o, l) in &note.previews {
        push_jpeg(data, o, l, info.orientation, &mut previews);
    }
    classify_previews(&mut previews);
    orientation::resolve_previews(data, &mut previews, info.orientation, active.width, active.height);
    meta.previews = previews;
    if let Some(c) = note.nef_compression {
        meta.set_extra("nef_compression", c);
    }
    if note.color_balance_version > 0 {
        meta.set_extra("color_balance_version", note.color_balance_version);
    }

    // --- The decoder ---
    let (width, height) = (buffer_width as usize, raw_h as usize);
    let row_layout = match &payload {
        Payload::Packed(s) => s.fast_layout().map(|layout| conventions::RowLayout { layout, shift: 0 }),
        Payload::Words { order, shift } => Some(conventions::RowLayout {
            layout: PackedLayout::tight(width, height, 16, pack_order(*order)),
            shift: *shift,
        }),
        Payload::PaddedLsb { bits, row_stride } => Some(conventions::RowLayout {
            layout: PackedLayout { width, height, bits: *bits, order: PackOrder::LsbBytes, row_stride: *row_stride },
            shift: 0,
        }),
        _ => None,
    };
    let packed_payload = match &payload {
        Payload::Packed(s) => s.fast_layout().filter(|l| l.order == PackOrder::MsbBytes).map(|l| PackedPayload {
            data: src,
            width: buffer_width,
            height: raw_h,
            bits: l.bits,
            order: l.order,
            row_stride: l.row_stride,
        }),
        _ => None,
    };
    let format =
        if model_uc.starts_with("COOLPIX ") && model_uc != "COOLPIX A" { RawFormat::Nrw } else { RawFormat::Nef };
    let nef = Arc::new(NefRaw {
        data,
        offset,
        src,
        declared: bytes,
        width,
        height,
        visible_rows,
        payload,
        huffman,
        bps,
        file_order: tiff.order,
    });
    let mut dec = SimpleDecoder::new(format, meta, info, Box::new(move |ctx| nef.decode(ctx)));
    dec.random_access = if row_layout.is_some() { RandomAccess::Rows } else { RandomAccess::Sequential };
    dec.units = vec![DecodeUnit { rect: Rect::full(buffer_width, raw_h), offset: offset as u64, length: bytes as u64 }];
    dec.packed = packed_payload;
    if let Some(rl) = row_layout {
        dec.region =
            Some(Box::new(move |ctx, r| conventions::decode_rows_region(data, offset, DataUnit::Strip(0), rl, r, ctx)));
    }
    Ok(Box::new(dec))
}

fn pack_order(order: ByteOrder) -> PackOrder {
    if order == ByteOrder::Little { PackOrder::LsbBytes } else { PackOrder::MsbBytes }
}

// ---------------------------------------------------------------------------------------------
// Decoding
// ---------------------------------------------------------------------------------------------

impl NefRaw<'_> {
    fn decode(&self, ctx: &DecodeContext) -> Result<RawImage> {
        let (w, h) = (self.width, self.height);
        let unit = DataUnit::Strip(0);
        let cpp = match &self.payload {
            Payload::SNef => 3,
            Payload::Coolscan { samples } => (*samples).max(1),
            _ => 1,
        };
        let mut img = RawImage::new_u16(w as u32, h as u32, cpp, &ctx.limits)?;
        let out = img.u16_mut();
        match &self.payload {
            Payload::HighEfficiency => {
                return Err(Error::unsupported_compression(
                    "NEF",
                    "Nikon high-efficiency (TicoRAW / JPEG XS) is a proprietary codec",
                ));
            }
            Payload::Huffman => {
                let meta = self.huffman.as_ref().ok_or_else(|| Error::corrupt("missing Nikon linearization table"))?;
                let rows = self.visible_rows;
                let dead_row = ctx
                    .profiler
                    .time(Stage::EntropyDecode, || {
                        nikon_huffman::decode_nikon_rows(self.src, meta, w, rows, out, w, ctx.parallel, self.declared)
                    })
                    .map_err(|e| e.or_truncated(self.data.len(), unit, self.offset, self.declared))?;
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
                let layout = PackedLayout::tight(w, h, 16, pack_order(*order));
                self.unpack_rows(ctx, layout, *shift, out)?;
            }
            Payload::PaddedLsb { bits, row_stride } => {
                let layout = PackedLayout {
                    width: w,
                    height: h,
                    bits: *bits,
                    order: PackOrder::LsbBytes,
                    row_stride: *row_stride,
                };
                self.unpack_rows(ctx, layout, 0, out)?;
            }
            Payload::SNef => self.decode_snef(ctx, out)?,
            Payload::Coolscan { .. } => self.decode_coolscan(ctx, cpp as usize, out)?,
        }
        Ok(img)
    }

    fn unpack_rows(&self, ctx: &DecodeContext, layout: PackedLayout, shift: u32, out: &mut [u16]) -> Result<()> {
        let w = self.width;
        let src = ctx.sensor_rows(
            self.data,
            DataUnit::Strip(0),
            self.offset,
            conventions::row_geometry(&layout, self.height),
        )?;
        ctx.profiler
            .time(Stage::Unpack, || packed::unpack(src, layout, out, w, ctx.parallel, ctx.simd, ctx.recover))?;
        if shift > 0 {
            par::for_each_row_chunk_bounded(out, w, par::memory_bound_workers(), ctx.parallel, |_, chunk| {
                for v in chunk {
                    *v >>= shift;
                }
            });
        }
        Ok(())
    }

    /// sNEF: YCbCr 4:2:2 pairs of pixels in six bytes, tone-mapped to 14-bit camera RGB.
    fn decode_snef(&self, ctx: &DecodeContext, out: &mut [u16]) -> Result<()> {
        let (w, h) = (self.width, self.height);
        if w < 2 {
            return Err(Error::corrupt("sNEF too narrow"));
        }
        let row_bytes = 3 * w;
        let geometry = RowGeometry { rows: h, stride: row_bytes, row_bytes, samples: row_bytes };
        let src = ctx.sensor_rows(self.data, DataUnit::Strip(0), self.offset, geometry)?;
        let tone = snef_tone_table();
        ctx.profiler.time(Stage::EntropyDecode, || {
            par::for_each_row_chunk_bounded(out, row_bytes, par::threads(), ctx.parallel, |first, chunk| {
                for (i, row) in chunk.chunks_mut(row_bytes).enumerate() {
                    let y = first + i;
                    let start = y * row_bytes;
                    if src.len() < start + row_bytes {
                        break;
                    }
                    snef_row(&src[start..start + row_bytes], row, tone);
                }
            })
        });
        Ok(())
    }

    /// Film-scanner payloads: 8-bit samples are stretched to 16 bits, 16-bit ones read in file order.
    fn decode_coolscan(&self, ctx: &DecodeContext, cpp: usize, out: &mut [u16]) -> Result<()> {
        let (w, h) = (self.width, self.height);
        let bits = if self.bps <= 8 { 8 } else { 16 };
        let row_samples = w * cpp;
        let src =
            ctx.sensor_rows(self.data, DataUnit::Strip(0), self.offset, RowGeometry::tight(h, row_samples, bits))?;
        let order = self.file_order;
        ctx.profiler.time(Stage::Unpack, || {
            par::for_each_row_chunk_bounded(
                out,
                row_samples,
                par::memory_bound_workers(),
                ctx.parallel,
                |first, chunk| {
                    let bytes_per = (bits / 8) as usize;
                    let from = (first * row_samples * bytes_per).min(src.len());
                    let src = &src[from..];
                    if bits == 8 {
                        for (o, &s) in chunk.iter_mut().zip(src) {
                            *o = stretch_8_to_16(s);
                        }
                    } else {
                        for (o, s) in chunk.iter_mut().zip(src.chunks_exact(2)) {
                            *o = match order {
                                ByteOrder::Little => u16::from_le_bytes([s[0], s[1]]),
                                ByteOrder::Big => u16::from_be_bytes([s[0], s[1]]),
                            };
                        }
                    }
                },
            )
        });
        Ok(())
    }
}

/// 8-bit scan sample to 16 bits: `floor(65536 * i / 255)`, with 255 giving 65535.
fn stretch_8_to_16(i: u8) -> u16 {
    if i == 255 { 65535 } else { (65536 * u32::from(i) / 255) as u16 }
}

// --- sNEF ------------------------------------------------------------------------------------

/// Entries of the sNEF tone table: the input is the colour in `0..=1` scaled by this.
const SNEF_TONE_SCALE: usize = 3072;
/// Largest output of the tone table (14-bit).
const SNEF_TONE_MAX: f64 = 16383.0;
/// Polynomial of the camera's tone curve `1 - exp(-(b1 x + b2 x^2 + b3 x^3 + b4 x^4))`.
const SNEF_TONE_COEFFS: [f64; 4] = [-5.79342238397656e-2, 3.28163551282665, -8.43136004842678, 1.03533181861023e1];
/// Luma divisor, chroma offset and divisor of the camera's YCbCr coding.
const SNEF_LUMA_DIV: f32 = 2549.0;
const SNEF_CHROMA_OFFSET: i32 = 1280;
const SNEF_CHROMA_DIV: f32 = 1536.0;
/// Above this luma the chroma is forced neutral.
const SNEF_NEUTRAL_ABOVE: f32 = 0.803;

fn snef_tone_table() -> &'static [u16] {
    static TABLE: OnceLock<Vec<u16>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let [b1, b2, b3, b4] = SNEF_TONE_COEFFS;
        (0..=SNEF_TONE_SCALE)
            .map(|i| {
                let x = i as f64 / SNEF_TONE_SCALE as f64;
                let exponent = -(b1 * x) - b2 * x * x - b3 * x * x * x - b4 * x * x * x * x;
                ((1.0 - exponent.exp()).max(0.0) * SNEF_TONE_MAX) as u16
            })
            .collect()
    })
}

/// One pixel: luma `y`, chroma `cb`, `cr` (12-bit integers) to three tone-mapped samples.
fn snef_pixel(y: u32, cb: u32, cr: u32, tone: &[u16]) -> [u16; 3] {
    let y = (y as f32 / SNEF_LUMA_DIV).min(1.0);
    let mut c2 = (cb as i32 - SNEF_CHROMA_OFFSET) as f32 / SNEF_CHROMA_DIV;
    let mut c3 = (cr as i32 - SNEF_CHROMA_OFFSET) as f32 / SNEF_CHROMA_DIV;
    if y > SNEF_NEUTRAL_ABOVE {
        c2 = 0.5;
        c3 = 0.5;
    }
    let r = (y + 1.402 * (c3 - 0.5)).clamp(0.0, 1.0);
    let g = (y - 0.34414 * (c2 - 0.5) - 0.71414 * (c3 - 0.5)).clamp(0.0, 1.0);
    let b = (y + 1.772 * (c2 - 0.5)).clamp(0.0, 1.0);
    let at = |v: f32| tone[(v * SNEF_TONE_SCALE as f32) as usize];
    [at(r), at(g), at(b)]
}

/// Convert one row: pairs of pixels share six bytes (two 12-bit lumas, one Cb, one Cr); the
/// second pixel of a pair takes the chroma halfway to the next pair's.
fn snef_row(line: &[u8], out: &mut [u16], tone: &[u16]) {
    // An odd width ends in half a pair, which has no data: it converts as all zeros.
    let pair = |k: usize| {
        let Some(b) = line.get(6 * k..6 * k + 6) else { return (0, 0, 0, 0) };
        let (b0, b1, b2, b3, b4, b5) =
            (u32::from(b[0]), u32::from(b[1]), u32::from(b[2]), u32::from(b[3]), u32::from(b[4]), u32::from(b[5]));
        ((b1 & 15) << 8 | b0, b2 << 4 | b1 >> 4, (b4 & 15) << 8 | b3, b5 << 4 | b4 >> 4)
    };
    let width = out.len() / 3;
    let pairs = width.div_ceil(2);
    if pairs == 0 {
        return;
    }
    let mut cur = pair(0);
    for k in 0..pairs {
        let next = if k + 1 < pairs { pair(k + 1) } else { cur };
        let first = snef_pixel(cur.0, cur.2, cur.3, tone);
        let second = snef_pixel(cur.1, (cur.2 + next.2) / 2, (cur.3 + next.3) / 2, tone);
        out[6 * k..6 * k + 3].copy_from_slice(&first);
        if let Some(px) = out.get_mut(6 * k + 3..6 * k + 6) {
            px.copy_from_slice(&second);
        }
        cur = next;
    }
}

#[cfg(test)]
mod tests;
