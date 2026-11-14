//! Sony ARW / SR2 / ARQ raw files: the container reader.
//!
//! This file classifies the raw IFD of a Sony TIFF, derives the frame geometry, levels, white balance and
//! colour data, and hands a [`payload::Payload`] to the payload decoders in `payload.rs`. The ARW1 DSLR-A100
//! layout is handled here too; Cyber-shot SRF files are `srf.rs`.

#[cfg(test)]
mod container_tests;
pub mod crypt;
pub(crate) mod model_id;
#[allow(dead_code)]
mod payload;
#[cfg(test)]
mod robustness;
mod sr2;
#[allow(dead_code)]
mod srf;
pub(crate) mod table;
mod visible;

use crate::common::*;
use flashraw_core::tiff::{Ifd, Tiff, tags};
use flashraw_core::*;
use payload::{Payload, build_decoder, payload_bytes, tile_grid};

/// Sony raws are dispatched by TIFF make, never by signature.
pub fn probe(_data: &[u8]) -> bool {
    false
}

pub fn open<'a>(data: &'a [u8], opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    let tiff = Tiff::parse(data)?;
    open_tiff(data, tiff, opts)
}

/// Sony private raw-IFD tags (see the public Sony tag lists).
mod sony_tags {
    pub const TONE_CURVE: u16 = 0x7010;
    pub const BLACK_RGBG: u16 = 0x7300;
    pub const WB_GRBG: u16 = 0x7303;
    pub const BLACK_RGGB: u16 = 0x7310;
    pub const WB_RGGB: u16 = 0x7313;
    pub const CROP_ORIGIN: u16 = 0x74C7;
    pub const CROP_SIZE: u16 = 0x74C8;
}

/// Open a Sony raw (also reached for Sony-built bodies sold under another make).
pub fn open_tiff<'a>(data: &'a [u8], tiff: Tiff<'a>, opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    let mut meta = RawMetadata::default();
    for ifd in &tiff.ifds {
        exif::read_ifd0(ifd, &mut meta);
    }
    name_placeholder_body(data, &tiff, &mut meta);
    record_image_size(data, &tiff, &mut meta);

    if srf::is_srf(&meta.model) {
        return srf::open(data, &tiff, meta, opts);
    }
    if meta.model == "DSLR-A100" {
        return open_a100(data, &tiff, meta, opts);
    }

    let raw = find_raw_ifd(&tiff)
        .ok_or_else(|| no_raw_ifd(&tiff, "Sony", Error::invalid_metadata("Sony: no raw image IFD")))?;
    let mut kind = classify(raw)?;
    let (raw_w, raw_h) = (raw.width(), raw.height());
    // The ARW1 stream carries eight more rows than the IFD announces.
    let (width, height) = (raw_w, if matches!(kind.payload, Payload::Arw1 { .. }) { raw_h + 8 } else { raw_h });
    opts.limits.check_dims(width, height, if kind.four_samples() { 4 } else { 1 })?;

    let model = meta.model.clone();
    let mut levels = Levels::from_tags(raw, &sr2::entries(data, &tiff));
    levels.fill_defaults(width, kind.bits, matches!(kind.payload, Payload::Arw2 { .. }));
    db_lookup(&mut meta, kind.mode);

    let entry = table::lookup("Sony", table_model(&model));
    let (mut black, mut cblack, mut white) = (levels.black.unwrap_or(0), levels.cblack, levels.white);
    table::apply_levels(entry, &mut black, &mut cblack, &mut white);
    let mut white = white.unwrap_or_else(|| (1u32 << kind.bits.min(16)) - 1);
    match kind.payload {
        Payload::Packed12 { .. } => {
            white = white.min(4095);
            if black > 256 || cblack[0] > 256 {
                black /= 4;
                cblack = cblack.map(|c| c / 4);
            }
        }
        // The decoded ARW6 scale is not the tags' (they state 512 / 16383): the HQ sample clips at
        // 39002 (71 image pixels) and no pixel of the four ARW6 samples (ISO 100) is below 1030.
        Payload::Arw6 => (black, cblack, white) = (1024, [0; 4], 39002),
        _ => {}
    }

    let (vw, vh) = visible::visible_size(&model, width, height, matches!(kind.payload, Payload::Arq));
    let active = Rect::new(0, 0, vw, vh);
    if let Payload::Arw1 { stored_rows, .. } = &mut kind.payload {
        *stored_rows = active.bottom() as usize;
    }

    let mut info = FrameInfo::new(width, height, kind.bits.min(16) as u8);
    info.compression = kind.compression;
    info.orientation = first_orientation(&tiff);
    info.active_area = active;
    info.crop = default_crop(raw).and_then(|c| c.intersect(&active)).unwrap_or(active);
    info.white_level = WhiteLevel::uniform(white);
    info.white_balance = levels.cam_mul.and_then(sane_wb);
    if kind.four_samples() {
        info.cpp = if matches!(kind.payload, Payload::Ycbcr { .. }) { 3 } else { 4 };
        info.cfa = None;
        info.layout = SensorLayout::Linear;
        info.black_level = BlackLevel::uniform(black as f32);
    } else {
        let cfa = tiff_ep_cfa(raw).filter(CfaPattern::is_bayer).unwrap_or(CfaPattern::RGGB);
        info.layout = layout_for(&cfa);
        info.black_level = cfa_black(&cfa, black, &cblack);
        info.cfa = Some(cfa);
    }
    if matches!(kind.payload, Payload::Ycbcr { .. }) {
        // The camera converted these tiles to RGB, removed the chroma offset and white balanced them.
        // The tags state 512, but the green channel of the three samples (ILCE-1M2, ILCE-7CR,
        // DSC-RX1RM3, ISO 100-125) never goes below 1004: the converted data sits on 1024. The
        // tags state a white of 16383, but the converted data of the ILCE-1M2 and ILCE-7CR samples
        // reaches 17337, so the stated level would clip it; 17536 lies above every sample value.
        info.black_level = BlackLevel::uniform(1024.0);
        info.white_level = WhiteLevel::uniform(17536);
        info.white_balance_applied = true;
    }
    info.color_matrices = table::color(entry, &meta);

    meta.previews = tiff_previews(data, 0, &tiff, info.orientation);
    let payload_ranges = match &kind.payload {
        Payload::LjpegTiles { tiles, .. } | Payload::Ycbcr { tiles, .. } => tiles.clone(),
        _ => strips(raw),
    };
    meta.previews.retain(|p| {
        !payload_ranges.iter().any(|&(o, l)| p.offset >= o as u64 && p.offset < (o as u64).saturating_add(l as u64))
    });

    let src = payload_bytes(data, raw, &kind.payload)?;
    build_decoder(data, meta, info, kind.payload, src, width, height)
}

/// Black-level pattern anchored at (0, 0): `black` plus the per-colour offsets `cblack` (R, G, B, G2 order),
/// laid out over the 2x2 cell of `cfa`. Also used by the Olympus reader.
pub(crate) fn cfa_black(cfa: &CfaPattern, black: u32, cblack: &[u32; 4]) -> BlackLevel {
    let mut greens = 0;
    let mut cell = [0f32; 4];
    for (i, v) in cell.iter_mut().enumerate() {
        let slot = match cfa.color_at(i % 2, i / 2) {
            CfaColor::Red => 0,
            CfaColor::Green => {
                greens += 1;
                if greens == 1 { 1 } else { 3 }
            }
            CfaColor::Blue => 2,
            _ => 1,
        };
        *v = black.wrapping_add(cblack[slot]) as f32;
    }
    if cell.iter().all(|&v| v == cell[0]) { BlackLevel::uniform(cell[0]) } else { BlackLevel::pattern2x2(cell) }
}

// ---------------------------------------------------------------------------------------------------------
// Identification

/// Pre-production firmware writes a placeholder IFD0 Model; the maker note's model id names the body.
fn name_placeholder_body(data: &[u8], tiff: &Tiff, meta: &mut RawMetadata) {
    if !model_id::is_placeholder(&meta.model) {
        return;
    }
    if let Some(name) = model_id::makernote_model_id(data, tiff).and_then(model_id::model_for_id) {
        let old = std::mem::replace(&mut meta.model, name.to_string());
        meta.set_extra("ifd0_model", old);
    }
}

/// The camera's image size: the maker note's native size, unless the EXIF size has another aspect ratio
/// (the camera shot a cropped ratio), in which case the smaller of the two in each dimension.
fn record_image_size(data: &[u8], tiff: &Tiff, meta: &mut RawMetadata) {
    let Some(full) = model_id::makernote_full_image_size(data, tiff) else { return };
    let size = camera_image_size(full, meta.image_size());
    meta.set_image_size(size.0, size.1);
}

/// The aspect ratios of the maker note's size and of the EXIF size agree to 3%: the maker note wins
/// (older bodies write a padded EXIF size); otherwise the camera shot another ratio and the EXIF size is
/// the image, never larger than the sensor's native size.
fn camera_image_size(full: (u32, u32), exif: Option<(u32, u32)>) -> (u32, u32) {
    let ratio = |(w, h): (u32, u32)| w as f64 / h.max(1) as f64;
    match exif {
        Some(exif) if (ratio(exif) / ratio(full) - 1.0).abs() > 0.03 => (exif.0.min(full.0), exif.1.min(full.1)),
        _ => full,
    }
}

/// Body models that share a fallback-table entry with a twin.
fn table_model(model: &str) -> &str {
    match model {
        "UMC-R10C" => "ILCE-QX1",
        "DSLR-A390" => "DSLR-A380",
        "ILCE-3500" => "ILCE-3000",
        other => other,
    }
}

/// The first non-default orientation of any IFD.
fn first_orientation(tiff: &Tiff) -> Orientation {
    tiff.all_ifds()
        .into_iter()
        .filter_map(|i| i.u32(tags::ORIENTATION))
        .map(Orientation::from_exif)
        .find(|&o| o != Orientation::Normal)
        .unwrap_or(Orientation::Normal)
}

// ---------------------------------------------------------------------------------------------------------
// The raw IFD and its payload

/// The IFD with the most sensor bits: the raw image, not a thumbnail or preview.
fn find_raw_ifd<'t, 'a>(tiff: &'t Tiff<'a>) -> Option<&'t Ifd<'a>> {
    let mut best: Option<(u64, &Ifd)> = None;
    for ifd in tiff.all_ifds() {
        if !could_be_raw(ifd) {
            continue;
        }
        let bits = u64::from(ifd.width()) * u64::from(ifd.height()) * u64::from(ifd.bps().max(1));
        if best.is_none_or(|(b, _)| bits > b) {
            best = Some((bits, ifd));
        }
    }
    best.map(|(_, i)| i)
}

fn could_be_raw(ifd: &Ifd) -> bool {
    let (w, h, bps) = (ifd.width(), ifd.height(), ifd.bps());
    let samples = ifd.u32(tags::SAMPLES_PER_PIXEL).unwrap_or(1);
    let (comp, photometric) = (ifd.compression(), ifd.u32(tags::PHOTOMETRIC).unwrap_or(0));
    let jpeg_preview = (comp == 6 && samples == 3 && bps < 12)
        || (bps == 8 && photometric == 6 && comp == 7 && samples == 3 && ifd.u32(tags::NEW_SUBFILE_TYPE) == Some(1));
    !(w == 0 || h == 0 || w > 65535 || h > 65535 || jpeg_preview)
        && bps <= 32
        && samples <= 12
        && (ifd.has(tags::STRIP_OFFSETS) || ifd.has(tags::TILE_OFFSETS))
        && !is_preview_ifd(ifd)
}

/// How the raw IFD's sensor data is stored.
struct Kind {
    payload: Payload,
    /// Camera-database mode name.
    mode: &'static str,
    /// Significant bits (drives the default levels).
    bits: u32,
    compression: Compression,
}

impl Kind {
    /// ARQ and the lossless YCbCr tiles carry several samples per pixel.
    fn four_samples(&self) -> bool {
        matches!(self.payload, Payload::Arq | Payload::Ycbcr { .. })
    }
}

/// TIFF compression values Sony uses for its own formats.
const COMP_ARW: u32 = 32767;
const COMP_ARW6: u32 = 32766;
const PHOTOMETRIC_CFA: u32 = 32803;

/// Tell the storage layouts apart. Sony reuses compression 32767 (and 1) for several layouts, so the
/// total strip byte count against the pixel count is the deciding fact.
fn classify(raw: &Ifd) -> Result<Kind> {
    let (w, h) = (u64::from(raw.width()), u64::from(raw.height()));
    let (bps, comp) = (raw.bps(), raw.compression());
    let samples = raw.u32(tags::SAMPLES_PER_PIXEL).unwrap_or(1);
    let photometric = raw.u32(tags::PHOTOMETRIC).unwrap_or(PHOTOMETRIC_CFA);
    let bytes = strips(raw).iter().fold(0u64, |a, &(_, l)| a.saturating_add(l as u64));
    let area = w * h;
    let kind = |payload, mode, bits, compression| Ok(Kind { payload, mode, bits, compression });
    let arw_or_raw = comp == COMP_ARW || comp == 1;

    if comp == COMP_ARW && bytes == area {
        let knots: Vec<u16> = raw
            .get(sony_tags::TONE_CURVE)
            .map(|e| e.u32s().into_iter().map(|v| v as u16).collect())
            .unwrap_or_default();
        return kind(
            Payload::Arw2 { curve: flashraw_codecs::sony_arw2::build_curve(&knots) },
            "arw2",
            14,
            Compression::SonyArw2,
        );
    }
    if arw_or_raw && bytes == 2 * area {
        // The DSC-R1 stores big-endian words.
        return kind(Payload::Unpacked16 { big_endian: w == 3984 }, "uncompressed", 14, Compression::Uncompressed);
    }
    if comp == 1 && samples == 4 && bytes == 8 * area {
        return kind(Payload::Arq, "arq", 14, Compression::Uncompressed);
    }
    if comp == COMP_ARW && bytes.saturating_mul(8) != area.saturating_mul(u64::from(bps)) {
        return kind(
            Payload::Arw1 { coded_rows: raw.height() as usize + 8, stored_rows: 0 },
            "arw1",
            12,
            Compression::SonyArw1,
        );
    }
    if arw_or_raw && bps == 12 {
        return kind(Payload::Packed12 { lsb: comp == COMP_ARW }, "12bit-packed", 12, Compression::Uncompressed);
    }
    if comp == 1 && (bps == 14 || bps == 16) {
        return kind(Payload::Unpacked16 { big_endian: w == 3984 }, "uncompressed", bps, Compression::Uncompressed);
    }
    if comp == COMP_ARW6 && photometric == PHOTOMETRIC_CFA && samples == 1 && (bps == 12 || bps == 14) {
        return kind(Payload::Arw6, "arw6", 14, Compression::Other);
    }
    if (comp == 6 || comp == 7) && photometric == 6 && samples == 3 {
        let (tiles, tile_w, tile_h) = tile_grid(raw)?;
        return kind(Payload::Ycbcr { tiles, tile_w, tile_h }, "lossless-ycbcr", 14, Compression::SonyLossless);
    }
    if (comp == 6 || comp == 7) && samples == 1 {
        let (tiles, tile_w, tile_h) = tile_grid(raw)?;
        return kind(
            Payload::LjpegTiles { tiles, tile_w, tile_h },
            "lossless",
            bps.clamp(8, 16),
            Compression::SonyLossless,
        );
    }
    Err(Error::unsupported_compression("ARW", format!("compression {comp}, {bps} bps, {samples} samples")))
}

/// The camera's own crop rectangle (Sony tags 0x74C7/0x74C8, else the DNG default crop tags).
fn default_crop(raw: &Ifd) -> Option<Rect> {
    let pair = |tags: [u16; 2]| {
        tags.into_iter().find_map(|t| {
            let e = raw.get(t)?;
            Some((e.u32(0)?, e.u32(1)?))
        })
    };
    let (x, y) = pair([sony_tags::CROP_ORIGIN, tags::DEFAULT_CROP_ORIGIN])?;
    let (w, h) = pair([sony_tags::CROP_SIZE, tags::DEFAULT_CROP_SIZE])?;
    (w > 0 && h > 0).then(|| Rect::new(x, y, w, h))
}

// ---------------------------------------------------------------------------------------------------------
// Black levels, white level and white balance

/// Levels as gathered from the tags; each source overrides the earlier ones.
#[derive(Default)]
struct Levels {
    black: Option<u32>,
    /// Offsets of R, G, B, G2 over `black`.
    cblack: [u32; 4],
    /// Multipliers R, G, B, G2.
    cam_mul: Option<[f32; 4]>,
    white: Option<u32>,
}

impl Levels {
    /// Gather from the raw IFD, then its DNG white level, then the SR2 private directory.
    fn from_tags(raw: &Ifd, sr2: &[sr2::Entry]) -> Levels {
        let mut l = Levels::default();
        for tag in [sony_tags::WB_GRBG, sony_tags::BLACK_RGGB, sony_tags::WB_RGGB] {
            if let Some(e) = raw.get(tag) {
                l.take(tag, &e.u32s());
            }
        }
        l.white = raw.u32(tags::WHITE_LEVEL);
        for (tag, values) in sr2 {
            let wide: Vec<u32> = values.iter().map(|&v| u32::from(v)).collect();
            l.take(*tag, &wide);
        }
        l
    }

    /// Apply one level tag; tags with fewer than four values are ignored.
    fn take(&mut self, tag: u16, v: &[u32]) {
        let &[a, b, c, d, ..] = v else { return };
        match tag {
            sony_tags::BLACK_RGBG => self.cblack = [a, b, c, d],
            sony_tags::BLACK_RGGB => {
                let cell = [a, b, d, c];
                let min = cell.into_iter().min().unwrap_or(0);
                self.black = Some(min);
                self.cblack = cell.map(|x| x - min);
            }
            sony_tags::WB_GRBG => self.cam_mul = Some([b as f32, a as f32, c as f32, d as f32]),
            sony_tags::WB_RGGB => self.cam_mul = Some([a as f32, b as f32, d as f32, c as f32]),
            _ => {}
        }
    }

    /// Wide-sensor ARW2 and 14-bit files that state no black level: 512, the value two other
    /// ILCE-7RM2 samples state. The ILCE-7RM2 ARW2 sample without a black tag has no image pixel
    /// below 450 at ISO 1600, the uncompressed 14-bit ILCE-7M2 sample none below 563.
    fn fill_defaults(&mut self, width: u32, bits: u32, arw2: bool) {
        if (arw2 || bits == 14) && self.black.unwrap_or(0) == 0 && self.cblack[0] == 0 && width > 3888 {
            self.black = Some(512);
        }
    }
}

/// Plausible multipliers only: positive, and no channel a hundred times another.
fn sane_wb(m: [f32; 4]) -> Option<WhiteBalance> {
    let rgb = &m[..3];
    let (lo, hi) = rgb.iter().fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), &v| (lo.min(v), hi.max(v)));
    if lo <= 0.001 || hi / lo > 100.0 {
        return None;
    }
    WhiteBalance::from_multipliers(m[0], m[1], m[2], m[3])
}

// ---------------------------------------------------------------------------------------------------------
// DSLR-A100

/// The A100 stores its raw stream behind IFD0's SubIFDs pointer; the geometry and white balance come from
/// an embedded Minolta MRW block.
fn open_a100<'a>(
    data: &'a [u8],
    tiff: &Tiff<'a>,
    mut meta: RawMetadata,
    opts: &OpenOptions,
) -> Result<Box<dyn RawDecoder + 'a>> {
    let ifd0 = tiff.ifds.first();
    let offset = ifd0
        .and_then(|i| i.u32(tags::SUB_IFDS))
        .ok_or_else(|| Error::invalid_metadata("A100: missing raw data pointer"))? as usize;
    let mrw = ifd0
        .and_then(|i| i.get(tags::DNG_PRIVATE_DATA))
        .and_then(|e| e.bytes().get(..4))
        .and_then(|b| crate::minolta::parse_blocks(data, tiff.order.u32([b[0], b[1], b[2], b[3]]) as usize).ok());
    let (raw_w, raw_h) = mrw
        .as_ref()
        .and_then(|m| m.prd.as_ref())
        .map_or((3880, 2608), |p| (u32::from(p.raw_width), u32::from(p.raw_height)));
    // The stream has one column more than the block announces.
    let (width, height) = (raw_w + 1, raw_h);
    opts.limits.check_dims(width, height, 1)?;
    let src = data.get(offset..).ok_or_else(|| Error::corrupt("A100: raw data offset beyond file"))?;
    db_lookup(&mut meta, "arw1");

    let entry = table::lookup("Sony", "DSLR-A100");
    let (mut black, mut cblack, mut white) = (0, [0; 4], None);
    table::apply_levels(entry, &mut black, &mut cblack, &mut white);

    let mut info = FrameInfo::new(width, height, 12);
    info.compression = Compression::SonyArw1;
    info.active_area = Rect::new(0, 0, width, height.saturating_sub(1));
    info.crop = info.active_area;
    info.cfa = Some(CfaPattern::GRBG);
    info.layout = SensorLayout::Bayer;
    info.orientation = first_orientation(tiff);
    info.black_level = BlackLevel::uniform(black as f32);
    info.white_level = WhiteLevel::uniform(white.unwrap_or(4095));
    info.white_balance = mrw.as_ref().and_then(|m| m.wb).and_then(sane_wb);
    info.color_matrices = table::color(entry, &meta);
    meta.previews = tiff_previews(data, 0, tiff, info.orientation);

    let (coded, stored) = (raw_h as usize, height.saturating_sub(1) as usize);
    let decode = move |ctx: &DecodeContext| payload::decode_arw1_stream(src, width, height, coded, stored, ctx);
    Ok(Box::new(SimpleDecoder::new(RawFormat::Arw, meta, info, Box::new(decode))))
}
