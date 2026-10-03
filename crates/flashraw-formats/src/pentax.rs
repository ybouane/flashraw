//! Pentax / Ricoh PEF (and Samsung GX-1/GX-10/GX-20, which are Pentax bodies).
//!
//! Payloads:
//! - TIFF compression 65535: Pentax Huffman ([`pentax_huffman`]) with the table from
//!   makernote tag 0x0220 (default table when absent);
//! - compression 32773 (mislabelled PackBits) or 1 with 12 bps: MSB-first 12-bit packing;
//! - 16-bit words (`*ist D`, and any file whose strip holds `w * h * 2` bytes).
//!
//! Makernote framing: `"AOC\0"` + byte order + IFD (offsets relative to the file) or
//! `"PENTAX \0"` + byte order + IFD (offsets relative to the makernote). Tags used:
//! 0x0200 black level (RGGB), 0x0201 as-shot WB (RGGB), 0x0220 Huffman table.
//!
//! Pixel-shift files (K-1, K-1 II, K-3 II, K-3 III, KP, K-70) store four frames; the

use crate::common::*;
use crate::nikon::conventions::{self, file_orientation, select_raw_ifd, wb_from_cam_mul};
use flashraw_codecs::nikon_huffman::DirectHuff;
use flashraw_codecs::nikon_packed::{self, PackedSpec};
use flashraw_codecs::packed::{self, PackedLayout};
use flashraw_codecs::pentax_huffman;
use flashraw_core::tiff::{Entry, Tiff, TiffOptions, parse_ifd_at, tags};
use flashraw_core::*;
use std::sync::Arc;

/// Magic-signature probe (PEF is TIFF based and dispatched by make).
pub fn probe(_data: &[u8]) -> bool {
    false
}

pub fn open<'a>(_data: &'a [u8], _opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    Err(Error::UnsupportedFormat("PEF files are TIFF based; use open_tiff".into()))
}

/// Values from the Pentax makernote.
#[derive(Debug, Default)]
struct MakerNote {
    order: Option<ByteOrder>,
    /// RGGB black levels (0x0200).
    black: Option<[u16; 4]>,
    /// RGGB as-shot multipliers (0x0201).
    wb: Option<[u16; 4]>,
    /// Absolute offset of the Huffman table (0x0220).
    huff_offset: Option<usize>,
    iso: Option<u32>,
}

fn parse_makernote(data: &[u8], entry: &Entry, file_order: ByteOrder) -> MakerNote {
    let mut mn = MakerNote::default();
    let pos = entry.value_pos;
    let Some(head) = data.get(pos..pos + 10) else { return mn };
    let opts = TiffOptions { follow_sub_ifds: false, ..TiffOptions::default() };
    let (base, order, ifd_off) = if head.starts_with(b"PENTAX \0") || head.starts_with(b"RICOH\0") {
        let o = if head.starts_with(b"PENTAX") { 8 } else { 6 };
        let order = match &head[o..o + 2] {
            b"II" => ByteOrder::Little,
            b"MM" => ByteOrder::Big,
            _ => return mn,
        };
        (pos, order, (o + 2) as u32)
    } else if head.starts_with(b"AOC\0") {
        (0, file_order, (pos + 6) as u32)
    } else {
        (0, file_order, pos as u32)
    };
    let slice = &data[base..];
    let Ok(ifd) = parse_ifd_at(slice, order, ifd_off, opts) else { return mn };
    mn.order = Some(order);
    let four = |e: &Entry| -> Option<[u16; 4]> { Some([e.u16(0)?, e.u16(1)?, e.u16(2)?, e.u16(3)?]) };
    for e in &ifd.entries {
        match e.tag {
            0x0014 => mn.iso = e.u32(0).map(pentax_iso).filter(|&v| v > 0),
            0x0200 => mn.black = four(e),
            0x0201 => mn.wb = four(e),
            0x0220 => mn.huff_offset = Some(base + e.value_pos),
            _ => {}
        }
    }
    mn
}

/// Pentax ISO code (makernote 0x0014) → ISO speed for the common codes.
fn pentax_iso(code: u32) -> u32 {
    const TABLE: [(u32, u32); 14] = [
        (3, 50),
        (4, 64),
        (5, 80),
        (6, 100),
        (7, 125),
        (8, 160),
        (9, 200),
        (10, 250),
        (11, 320),
        (12, 400),
        (15, 800),
        (18, 1600),
        (21, 3200),
        (24, 6400),
    ];
    TABLE.iter().find(|(c, _)| *c == code).map(|(_, v)| *v).unwrap_or(if code >= 50 { code } else { 0 })
}

#[derive(Debug, Clone)]
enum Payload {
    Huffman(Arc<DirectHuff>),
    Packed(PackedSpec),
    Words(ByteOrder),
}

struct PefRaw<'a> {
    src: &'a [u8],
    /// Payload length the strip declares (`src` may run on).
    declared: usize,
    width: usize,
    height: usize,
    payload: Payload,
}

/// Open a PEF (or a Pentax-built Samsung GX file).
pub fn open_tiff<'a>(data: &'a [u8], tiff: Tiff<'a>, opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    let ifd0 = tiff.ifds.first().ok_or_else(|| Error::corrupt("PEF without IFD0"))?;
    let mut meta = RawMetadata::default();
    flashraw_core::exif::read_ifd0(ifd0, &mut meta);
    // Ricoh-era bodies use the make "RICOH IMAGING COMPANY, LTD." with "PENTAX ..." models.
    let model = strip_make_prefix(&strip_make_prefix(&meta.model, "PENTAX"), "RICOH");
    let model_uc = model.to_ascii_uppercase();
    let mn = tiff.find_entry(tags::MAKER_NOTE).map(|e| parse_makernote(data, e, tiff.order)).unwrap_or_default();
    if meta.exposure.iso.is_none() {
        meta.exposure.iso = mn.iso;
    }

    let raw = select_raw_ifd(&tiff).ok_or_else(|| Error::invalid_metadata("PEF has no raw image IFD"))?;
    let (w, h) = (raw.width(), raw.height());
    opts.limits.check_dims(w, h, 1)?;
    let bps = raw.bps();
    let (offset, bytes) = *strips(raw).first().ok_or_else(|| Error::invalid_metadata("PEF raw IFD without strips"))?;
    let src = data.get(offset..).filter(|s| !s.is_empty()).ok_or_else(|| Error::corrupt("PEF payload beyond EOF"))?;
    let (w64, h64) = (w as u64, h as u64);
    let payload = match raw.compression() {
        65535 => {
            let order = mn.order.unwrap_or(tiff.order);
            let t = match mn.huff_offset {
                Some(o) => pentax_huffman::table_from_makernote(data.get(o..).unwrap_or(&[]), order)?,
                None => pentax_huffman::default_table()?,
            };
            Payload::Huffman(Arc::new(t))
        }
        1 | 32773 if bytes as u64 == w64 * h64 * 2 => Payload::Words(tiff.order),
        1 | 32773 if bps == 16 || bps == 14 => Payload::Words(tiff.order),
        1 | 32773 if (1..=16).contains(&bps) => Payload::Packed(PackedSpec {
            width: w as usize,
            height: h as usize,
            bits: bps,
            flags: 0,
            field2_offset: None,
        }),
        c => return Err(Error::unsupported_compression("PEF", format!("TIFF compression {c} ({bps} bps)"))),
    };

    // --- Visible area (reference conventions, by model and buffer size) ---
    let mut area = Rect::full(w, h);
    let mut cfa_visible: Option<CfaPattern> = None; // pattern at the visible origin
    let is_m = |m: &str| model_uc == m;
    if h == 2624 && w == 3936 {
        area = Rect::new(0, 0, 3896, 2616); // K10D, K200D, K-m, K2000, GX10
    }
    if h == 3136 && w == 4864 {
        area = Rect::new(0, 0, 4688, 3124); // K20D, GX20
        cfa_visible = Some(CfaPattern::BGGR);
    }
    let monochrome = raw.u32(tags::PHOTOMETRIC) == Some(34892) || model_uc.contains("MONOCHROME");
    if w == 4352 && (is_m("K-R") || is_m("K-X")) {
        area.width = 4309;
        cfa_visible = Some(CfaPattern::BGGR);
    }
    if w >= 4960 && (is_m("K-5") || is_m("K-5 II") || is_m("K-5 II S")) {
        area.x = 10;
        area.width = 4950;
        cfa_visible = Some(CfaPattern::BGGR);
    }
    if w == 6080 && is_m("K-70") {
        area = Rect::new(60, 32, 6020, 4016);
    }
    if w == 4736 && is_m("K-7") {
        area = Rect::new(0, 2, 4684, 3122);
        cfa_visible = Some(CfaPattern::BGGR);
    }
    if w == 6080 && is_m("KF") {
        area = Rect::new(56, 28, 6024, 4024);
    }
    if w == 6080 && (is_m("K-3 II") || is_m("K-3")) {
        area.x = 4;
        area.width = 6040;
    }
    if w == 6304 && model_uc.starts_with("K-3 MARK III") {
        area = Rect::new(26, 34, 6224, 4160);
    }
    if w == 6112 && is_m("KP") {
        area = Rect::new(54, 28, 6028, h.saturating_sub(28));
    }
    if w == 7424 && is_m("645D") {
        area = Rect::new(48, 29, 7328, 5502);
        cfa_visible = Some(CfaPattern::GRBG);
    }
    if is_m("K-1") || is_m("K-1 MARK II") {
        area.y = 18;
        area.height = h.saturating_sub(18);
        if w == 7392 {
            area.x = 6;
            area.width = 7376;
        }
    }
    if is_m("*IST DS") {
        area.height = area.height.saturating_sub(2);
    }
    let cfa_override = cfa_visible.map(|p| conventions::cfa_at_buffer_origin(p, area.x, area.y));
    let area = conventions::even_margins(area);

    // --- Levels ---
    let mode = match &payload {
        Payload::Huffman(_) => "compressed",
        _ => "",
    };
    let cam = db_lookup(&mut meta, mode);
    let table_model = if model_uc == "K2000" { "K-m".to_string() } else { meta.canonical_model.clone() };
    let table = flashraw_camera_db::find_matrix("Pentax", &table_model)
        .or_else(|| flashraw_camera_db::find_matrix("Pentax", &model));
    let (mut base_black, mut cblack) = (0u32, [0u32; 4]);
    if let Some(b) = mn.black {
        cblack = b.map(|v| v as u32);
    }
    let mut maximum = 0u32;
    conventions::apply_table_levels(table, &mut base_black, &mut cblack, &mut maximum);
    if maximum == 0 {
        maximum = (1u32 << bps.clamp(1, 16)) - 1;
    }

    let mut info = FrameInfo::new(w, h, bps.min(16) as u8);
    info.compression = match &payload {
        Payload::Huffman(_) => Compression::PentaxHuffman,
        _ => Compression::Uncompressed,
    };
    info.active_area = area;
    info.crop = area;
    info.white_level = WhiteLevel::uniform(maximum);
    info.orientation = file_orientation(&tiff);
    if monochrome {
        info.layout = SensorLayout::Monochrome;
        info.cfa = None;
        info.black_level = BlackLevel::uniform((base_black + cblack[0]) as f32);
    } else {
        let cfa = cfa_override
            .or_else(|| tiff_ep_cfa(raw))
            .or_else(|| cam.and_then(|c| c.cfa).and_then(parse_db_cfa))
            .unwrap_or(CfaPattern::RGGB);
        info.cfa = Some(cfa);
        info.layout = layout_for(&cfa);
        info.black_level = conventions::black_for_cfa(&cfa, base_black, cblack);
        info.white_balance = mn.wb.and_then(|m| wb_from_cam_mul([m[0] as f32, m[1] as f32, m[3] as f32, m[2] as f32]));
    }
    info.color_matrices = conventions::color_from_table(table, &meta, cam);
    meta.previews = tiff_previews(data, 0, &tiff, info.orientation);

    let pef = Arc::new(PefRaw { src, declared: bytes, width: w as usize, height: h as usize, payload });
    let p2 = pef.clone();
    let packed = match &pef.payload {
        Payload::Packed(s) => s.fast_layout().map(|l| PackedPayload {
            data: src,
            width: w,
            height: h,
            bits: l.bits,
            order: l.order,
            row_stride: l.row_stride,
        }),
        _ => None,
    };
    let row_layout = match &pef.payload {
        Payload::Packed(s) => s.fast_layout(),
        Payload::Words(o) => Some(PackedLayout::tight(
            w as usize,
            h as usize,
            16,
            if *o == ByteOrder::Little { PackOrder::LsbBytes } else { PackOrder::MsbBytes },
        )),
        Payload::Huffman(_) => None,
    }
    .map(|layout| conventions::RowLayout { layout, shift: 0 });
    let random_access = if row_layout.is_some() { RandomAccess::Rows } else { RandomAccess::Sequential };
    let mut dec = SimpleDecoder::new(RawFormat::Pef, meta, info, Box::new(move |ctx| p2.decode(ctx)));
    dec.random_access = random_access;
    dec.units = vec![DecodeUnit { rect: Rect::full(w, h), offset: offset as u64, length: bytes as u64 }];
    dec.packed = packed;
    if let Some(rl) = row_layout {
        dec.region = Some(Box::new(move |ctx, r| conventions::decode_rows_region(src, rl, r, ctx)));
    }
    Ok(Box::new(dec))
}

impl PefRaw<'_> {
    fn decode(&self, ctx: &DecodeContext) -> Result<RawImage> {
        let (w, h) = (self.width, self.height);
        let mut img = RawImage::new_u16(w as u32, h as u32, 1, &ctx.limits)?;
        let out = img.u16_mut();
        match &self.payload {
            Payload::Huffman(t) => {
                ctx.profiler.time(Stage::EntropyDecode, || {
                    pentax_huffman::decode(self.src, t, w, h, out, w, ctx.parallel, self.declared)
                })?;
            }
            Payload::Packed(spec) => {
                ctx.profiler
                    .time(Stage::Unpack, || nikon_packed::decode(self.src, spec, out, w, ctx.parallel, ctx.simd))?;
            }
            Payload::Words(order) => {
                let po = if *order == ByteOrder::Little { PackOrder::LsbBytes } else { PackOrder::MsbBytes };
                let layout = PackedLayout::tight(w, h, 16, po);
                ctx.profiler
                    .time(Stage::Unpack, || packed::unpack(self.src, layout, out, w, ctx.parallel, ctx.simd, false))?;
            }
        }
        Ok(img)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nikon::conventions::robustness::{corpus_file, mutate, open_decode};

    #[test]
    fn iso_codes() {
        assert_eq!(pentax_iso(6), 100);
        assert_eq!(pentax_iso(18), 1600);
        assert_eq!(pentax_iso(400), 400);
        assert_eq!(pentax_iso(1), 0);
    }

    /// One corpus sample per payload variant: originals decode, truncated/corrupted
    /// copies never panic.
    #[test]
    fn malformed_inputs_do_not_panic() {
        let samples = [
            "Pentax/K10D/_IGP7284.PEF",
            "Pentax/K100D/IMGP1754.PEF",
            "Pentax/ist D/pentax.PEF",
            "Pentax/K-3 Mark III Monochrome/K3III.PEF",
            "SAMSUNG TECHWIN/GX-1L/SG1L5458.PEF",
        ];
        for (i, s) in samples.iter().enumerate() {
            let Some(data) = corpus_file(s) else { continue };
            open_decode(&data).unwrap_or_else(|e| panic!("{s}: {e}"));
            mutate(&data, i as u64 * 104729 + 3);
        }
    }

    /// Region decodes of row-addressable payloads equal crops of full decodes.
    #[test]
    fn region_decode_matches_full_decode() {
        use crate::nikon::conventions::robustness::{corpus_file, region_matches_full};
        for s in ["Pentax/K100D/IMGP1754.PEF", "Pentax/ist D/pentax.PEF"] {
            let Some(data) = corpus_file(s) else { continue };
            assert!(region_matches_full(&data), "{s} should be row addressable");
        }
    }
}
