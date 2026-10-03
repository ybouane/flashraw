//! Samsung SRW (and Samsung-branded Pentax PEF files, which are forwarded to the
//! Pentax decoder).
//!
//! Payloads (raw IFD compression):
//! - 32769: uncompressed — 16-bit little-endian words (EX1, EX2F, WB2000) or packed;
//! - 32770: Samsung V0 when the raw IFD has tag 40976 (row offset table), otherwise
//!   12-bit packed (NX5/10/11/100: MSB first; NX20/200/210/1000/1100: LSB first);
//! - 32772: Samsung V1 (Huffman);
//! - 32773: Samsung V2 (NX1, NX500).
//!
//! The makernote is a plain IFD (offsets relative to its start). WB (0xA021) and black
//! levels (0xA028) are stored RGGB, offset by the per-file keys of tag 0xA020.

use crate::common::*;
use crate::nikon::conventions::{self, file_orientation, select_raw_ifd, wb_from_cam_mul};
use flashraw_codecs::nikon_packed::{self, PackedSpec};
use flashraw_codecs::packed::{self, PackedLayout};
use flashraw_codecs::samsung as codec;
use flashraw_core::bytes::ReadAt;
use flashraw_core::tiff::{Entry, Tiff, TiffOptions, parse_ifd_at, tags};
use flashraw_core::*;
use std::sync::Arc;

/// Magic-signature probe (SRW is TIFF based and dispatched by make).
pub fn probe(_data: &[u8]) -> bool {
    false
}

pub fn open<'a>(_data: &'a [u8], _opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    Err(Error::UnsupportedFormat("SRW files are TIFF based; use open_tiff".into()))
}

/// Tag 40976: offset of the per-row offset table of V0 data.
const TAG_SAMSUNG_ROW_OFFSETS: u16 = 0xa010;

#[derive(Debug, Default)]
struct MakerNote {
    /// As-shot multipliers R, G, B, G2.
    cam_mul: Option<[f32; 4]>,
    /// RGGB black levels.
    black: Option<[u32; 4]>,
}

fn parse_makernote(data: &[u8], entry: &Entry, order: ByteOrder) -> MakerNote {
    let mut mn = MakerNote::default();
    let base = entry.value_pos;
    let Some(slice) = data.get(base..) else { return mn };
    let opts = TiffOptions { follow_sub_ifds: false, ..TiffOptions::default() };
    let Ok(ifd) = parse_ifd_at(slice, order, 0, opts) else { return mn };
    let mut key = [0u32; 11];
    let vals = |e: &Entry, n: usize| -> Option<Vec<u32>> {
        let v: Vec<u32> = (0..n).map(|i| e.data.u32_at(order, i * 4).ok()).collect::<Option<_>>()?;
        Some(v)
    };
    for e in &ifd.entries {
        match e.tag {
            0xa020 => {
                if let Some(v) = vals(e, 11) {
                    key.copy_from_slice(&v);
                }
            }
            0xa021 => {
                if let Some(v) = vals(e, 4) {
                    let m: Vec<f32> = (0..4).map(|c| v[c].wrapping_sub(key[c]) as f32).collect();
                    // RGGB → R, G, B, G2
                    mn.cam_mul = Some([m[0], m[1], m[3], m[2]]);
                }
            }
            0xa028 => {
                if let Some(v) = vals(e, 4) {
                    mn.black = Some([0, 1, 2, 3].map(|c| v[c].wrapping_sub(key[c])));
                }
            }
            _ => {}
        }
    }
    mn
}

#[derive(Debug, Clone)]
enum Payload {
    V0 {
        row_offsets: Vec<u32>,
    },
    V1,
    V2,
    Packed(PackedSpec),
    /// 16-bit words shifted right by `shift`.
    Words(ByteOrder, u32),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum PayloadKind {
    Packed,
    Other,
}

struct SrwRaw<'a> {
    src: &'a [u8],
    width: usize,
    height: usize,
    payload: Payload,
}

/// Open an SRW (or forward a Samsung GX PEF to the Pentax decoder).
pub fn open_tiff<'a>(data: &'a [u8], tiff: Tiff<'a>, opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    // Samsung GX-1/GX-10/GX-20 are Pentax bodies writing PEF ("AOC" makernotes).
    let pentax_like =
        tiff.find_entry(tags::MAKER_NOTE).is_some_and(|e| data.get(e.value_pos..e.value_pos + 4) == Some(b"AOC\0"));
    if pentax_like {
        return crate::pentax::open_tiff(data, tiff, opts);
    }
    let ifd0 = tiff.ifds.first().ok_or_else(|| Error::corrupt("SRW without IFD0"))?;
    let mut meta = RawMetadata::default();
    flashraw_core::exif::read_ifd0(ifd0, &mut meta);
    let model = strip_make_prefix(&meta.model, "SAMSUNG");
    let model_uc = model.to_ascii_uppercase();
    let mn = tiff.find_entry(tags::MAKER_NOTE).map(|e| parse_makernote(data, e, tiff.order)).unwrap_or_default();

    let raw = select_raw_ifd(&tiff).ok_or_else(|| Error::invalid_metadata("SRW has no raw image IFD"))?;
    let (w, h) = (raw.width(), raw.height());
    opts.limits.check_dims(w, h, 1)?;
    let bps = raw.bps();
    let (offset, bytes) = *strips(raw).first().ok_or_else(|| Error::invalid_metadata("SRW raw IFD without strips"))?;
    let src = data.get(offset..).filter(|s| !s.is_empty()).ok_or_else(|| Error::corrupt("SRW payload beyond EOF"))?;
    let (w64, h64, b64) = (w as u64, h as u64, bytes as u64);
    let row_table = raw.u32(TAG_SAMSUNG_ROW_OFFSETS);
    let packed12 = |flags| {
        Payload::Packed(PackedSpec { width: w as usize, height: h as usize, bits: bps, flags, field2_offset: None })
    };
    let mut payload = match raw.compression() {
        32770 if row_table.is_some() => {
            let t = row_table.unwrap_or(0) as usize;
            let mut offs = Vec::with_capacity(h as usize);
            for r in 0..h as usize {
                let o = data
                    .u32_at(ByteOrder::Little, t + r * 4)
                    .map_err(|_| Error::corrupt("SRW row offset table truncated"))?;
                offs.push(o);
            }
            Payload::V0 { row_offsets: offs }
        }
        32772 => Payload::V1,
        32773 => Payload::V2,
        c @ (32769 | 32770 | 1) => {
            if b64 == w64 * h64 * 2 || bps > 12 {
                // 12-bit data in 16-bit words (compression 32769) is stored shifted left
                // by one bit (EX2F); 14-bit data is not.
                Payload::Words(tiff.order, if c == 32769 && bps == 12 { 1 } else { 0 })
            } else if bps == 12 {
                packed12(if c == 32769 { 1 } else { 0 })
            } else {
                return Err(Error::unsupported_compression("SRW", format!("uncompressed {bps}-bit")));
            }
        }
        c => return Err(Error::unsupported_compression("SRW", format!("TIFF compression {c}"))),
    };

    // --- Visible area and quirks (per-camera conventions by model/buffer size) ---
    let mut area = Rect::full(w, h);
    let mut cfa_visible: Option<CfaPattern> = None;
    if w == 4704 {
        // NX5, NX10, NX11, NX100
        area = Rect::new(8, 8, w.saturating_sub(16), h.saturating_sub(8));
    } else if model_uc == "NX3000" {
        area = Rect::new(92, 38, 5456, 3634);
        cfa_visible = Some(CfaPattern::GRBG);
    } else if h == 3714 {
        // NX2000, NX300(M), NX30, NX U / Galaxy NX family
        area = if w == 5600 {
            Rect::new(w.saturating_sub(5536), 18, 5536, h.saturating_sub(18))
        } else {
            Rect::new(0, 0, 5536, h.saturating_sub(18))
        };
        cfa_visible = Some(CfaPattern::GRBG);
    } else if w == 5632 {
        // NX20, NX200, NX210, NX1000, NX1100: LSB-first 12-bit packing.
        let left = 32 + bps.min(32);
        area = Rect::new(left, 2, 5574u32.saturating_sub(left), 3694);
        if bps == 12 {
            if let Payload::Packed(s) = &mut payload {
                s.flags = 80;
            }
        }
    } else if w == 5664 {
        // NX mini
        area = Rect::new(96, 17, 5544, h.saturating_sub(17));
        cfa_visible = Some(CfaPattern::GBRG);
    } else if w == 6496 {
        // NX1, NX500
        cfa_visible = Some(CfaPattern::GRBG);
    } else if model_uc == "EX1" {
        let mut aw = w.saturating_sub(6);
        let mut ah = h.saturating_sub(20);
        let mut top = 2;
        if aw > 3682 {
            ah = ah.saturating_sub(10);
            aw -= 46;
            top = 8;
        }
        area = Rect::new(0, top, aw, ah);
        payload = Payload::Words(ByteOrder::Little, 0);
    } else if model_uc == "WB2000" {
        let mut aw = w.saturating_sub(10);
        let mut ah = h.saturating_sub(3);
        let mut top = 2;
        if aw > 3718 {
            ah = ah.saturating_sub(28);
            aw -= 56;
            top = 8;
        }
        area = Rect::new(0, top, aw, ah);
        payload = Payload::Words(ByteOrder::Little, 0);
    } else if model_uc == "EX2F" {
        area = Rect::new(24, 15, 4040, 3030);
        cfa_visible = Some(CfaPattern::GBRG);
        let shift = if let Payload::Words(_, s) = payload { s } else { 1 };
        payload = Payload::Words(ByteOrder::Little, shift);
    } else if model_uc == "WB550" {
        if let Payload::Words(o, _) = &mut payload {
            *o = ByteOrder::Little;
        }
    }
    let area = area.clamp_to(w, h);
    let cfa_override = cfa_visible.map(|p| conventions::cfa_at_buffer_origin(p, area.x, area.y));
    let area = conventions::even_margins(area);

    // --- Levels ---
    let mode = match &payload {
        Payload::V0 { .. } | Payload::V1 | Payload::V2 => "compressed",
        _ => "",
    };
    let cam = db_lookup(&mut meta, mode);
    let table = flashraw_camera_db::find_matrix("Samsung", &meta.canonical_model)
        .or_else(|| flashraw_camera_db::find_matrix("Samsung", &model));
    let (mut base_black, mut cblack) = (0u32, mn.black.unwrap_or([0; 4]));
    let mut maximum = 0u32;
    conventions::apply_table_levels(table, &mut base_black, &mut cblack, &mut maximum);
    if w == 6496 && base_black == 0 && cblack == [0; 4] {
        base_black = 1 << bps.saturating_sub(7).min(15);
    }
    if maximum == 0 {
        maximum = (1u32 << bps.clamp(1, 16)) - 1;
    }

    let info_payload_kind =
        if matches!(payload, Payload::Packed(_)) { PayloadKind::Packed } else { PayloadKind::Other };
    let mut info = FrameInfo::new(w, h, bps.min(16) as u8);
    info.compression = match &payload {
        Payload::V0 { .. } => Compression::SamsungV0,
        Payload::V1 => Compression::SamsungV1,
        Payload::V2 => Compression::SamsungV2,
        _ => Compression::Uncompressed,
    };
    info.active_area = area;
    info.crop = area;
    info.white_level = WhiteLevel::uniform(maximum);
    info.orientation = file_orientation(&tiff);
    let cfa = cfa_override
        .or_else(|| tiff_ep_cfa(raw))
        .or_else(|| cam.and_then(|c| c.cfa).and_then(parse_db_cfa))
        .unwrap_or(CfaPattern::RGGB);
    info.cfa = Some(cfa);
    info.layout = layout_for(&cfa);
    info.black_level = conventions::black_for_cfa(&cfa, base_black, cblack);
    if w == 4704 && matches!(info_payload_kind, PayloadKind::Packed) {
        // NX5/10/11/100: the black level is measured on the optical-black columns left
        // and right of the visible area (integer means).
        // The columns are read straight from the packed rows: 16 samples per row.
        let right = area.x + area.width;
        info.masked_areas = vec![
            Rect::new(0, area.y, area.x, area.height),
            Rect::new(right, area.y, w.saturating_sub(right), area.height),
        ];
        if let Some(b) = masked_black_msb12(src, w as usize, &info.masked_areas) {
            info.black_level = b;
        }
    }
    info.white_balance = mn.cam_mul.and_then(wb_from_cam_mul);
    info.color_matrices = conventions::color_from_table(table, &meta, cam);
    meta.previews = tiff_previews(data, 0, &tiff, info.orientation);

    let packed = match &payload {
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
    let row_layout = match &payload {
        Payload::Packed(s) => s.fast_layout().map(|layout| conventions::RowLayout { layout, shift: 0 }),
        Payload::Words(o, shift) => Some(conventions::RowLayout {
            layout: PackedLayout::tight(
                w as usize,
                h as usize,
                16,
                if *o == ByteOrder::Little { PackOrder::LsbBytes } else { PackOrder::MsbBytes },
            ),
            shift: *shift,
        }),
        _ => None,
    };
    let random_access = if row_layout.is_some() { RandomAccess::Rows } else { RandomAccess::Sequential };
    // V0 rows are addressed relative to the strip offset; keep the whole file.
    let srw = Arc::new(SrwRaw { src, width: w as usize, height: h as usize, payload });
    let s2 = srw.clone();
    let mut dec = SimpleDecoder::new(RawFormat::Srw, meta, info, Box::new(move |ctx| s2.decode(ctx)));
    dec.random_access = random_access;
    dec.units = vec![DecodeUnit { rect: Rect::full(w, h), offset: offset as u64, length: bytes as u64 }];
    dec.packed = packed;
    if let Some(rl) = row_layout {
        dec.region = Some(Box::new(move |ctx, r| conventions::decode_rows_region(src, rl, r, ctx)));
    }
    Ok(Box::new(dec))
}

/// Per-CFA-position integer mean of the masked areas of an MSB-first 12-bit packed
/// image (rows of `width * 3 / 2` bytes). Returns `None` when the areas are mostly
/// zero (the makernote black is then kept).
fn masked_black_msb12(src: &[u8], width: usize, areas: &[Rect]) -> Option<BlackLevel> {
    let stride = width * 3 / 2;
    let mut sum = [0u64; 4];
    let mut cnt = [0u64; 4];
    let mut zero = 0u64;
    for r in areas {
        for y in r.y as usize..r.bottom() as usize {
            let row = src.get(y * stride..(y + 1) * stride)?;
            for x in r.x as usize..r.right() as usize {
                let b = x / 2 * 3;
                let s = row.get(b..b + 3)?;
                let v = if x % 2 == 0 {
                    ((s[0] as u16) << 4) | (s[1] as u16 >> 4)
                } else {
                    ((s[1] as u16 & 0xf) << 8) | s[2] as u16
                };
                let i = (y % 2) * 2 + x % 2;
                sum[i] += v as u64;
                cnt[i] += 1;
                zero += (v == 0) as u64;
            }
        }
    }
    if cnt.contains(&0) || zero >= cnt.iter().sum::<u64>() / 4 {
        return None;
    }
    let v: Vec<f32> = (0..4).map(|i| (sum[i] / cnt[i]) as f32).collect();
    Some(BlackLevel::pattern2x2([v[0], v[1], v[2], v[3]]))
}

impl SrwRaw<'_> {
    fn decode(&self, ctx: &DecodeContext) -> Result<RawImage> {
        let (w, h) = (self.width, self.height);
        let mut img = RawImage::new_u16(w as u32, h as u32, 1, &ctx.limits)?;
        let out = img.u16_mut();
        match &self.payload {
            Payload::V0 { row_offsets } => ctx
                .profiler
                .time(Stage::EntropyDecode, || codec::decode_v0_with(self.src, row_offsets, w, h, out, ctx.parallel))?,
            Payload::V1 => {
                ctx.profiler.time(Stage::EntropyDecode, || codec::decode_v1(self.src, w, h, out))?;
            }
            Payload::V2 => ctx.profiler.time(Stage::EntropyDecode, || codec::decode_v2(self.src, w, h, out))?,
            Payload::Packed(spec) => ctx
                .profiler
                .time(Stage::Unpack, || nikon_packed::decode(self.src, spec, out, w, ctx.parallel, ctx.simd))?,
            Payload::Words(order, shift) => {
                let po = if *order == ByteOrder::Little { PackOrder::LsbBytes } else { PackOrder::MsbBytes };
                let layout = PackedLayout::tight(w, h, 16, po);
                ctx.profiler
                    .time(Stage::Unpack, || packed::unpack(self.src, layout, out, w, ctx.parallel, ctx.simd, false))?;
                if *shift > 0 {
                    let s = *shift;
                    par::for_each_row_chunk(out, w, 256, ctx.parallel, |_, c| c.iter_mut().for_each(|v| *v >>= s));
                }
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
    fn masked_black_needs_data() {
        let areas = [Rect::new(0, 0, 2, 2)];
        assert!(masked_black_msb12(&[], 16, &areas).is_none());
        // 4 samples of 0x101 per row (12-bit MSB packing: 10 11 01 10 11 01 ...).
        let row: Vec<u8> = [0x10, 0x11, 0x01].repeat(8);
        let data = [row.clone(), row].concat();
        let b = masked_black_msb12(&data, 16, &areas).unwrap();
        assert_eq!(b.values, vec![257.0; 4]);
    }

    /// One corpus sample per payload variant: originals decode, truncated/corrupted
    /// copies never panic.
    #[test]
    fn malformed_inputs_do_not_panic() {
        let samples = [
            "Samsung/NX10/SAM_3630.SRW",
            "Samsung/NX1000/_5169011.SRW",
            "Samsung/NX300/nx300.SRW",
            "Samsung/NX mini/SAM_0295.SRW",
            "Samsung/NX500/SAM_2927.SRW",
            "Samsung/EX1/SAM_5548.SRW",
            "Samsung/EX2F/RAW_SAMSUNG_EX2F.SRW",
        ];
        for (i, s) in samples.iter().enumerate() {
            let Some(data) = corpus_file(s) else { continue };
            open_decode(&data).unwrap_or_else(|e| panic!("{s}: {e}"));
            mutate(&data, i as u64 * 15485863 + 5);
        }
    }

    /// Region decodes of row-addressable payloads equal crops of full decodes.
    #[test]
    fn region_decode_matches_full_decode() {
        use crate::nikon::conventions::robustness::{corpus_file, region_matches_full};
        for s in [
            "Samsung/NX10/SAM_3630.SRW",
            "Samsung/NX1000/_5169011.SRW",
            "Samsung/EX2F/RAW_SAMSUNG_EX2F.SRW",
            "Samsung/EX1/SAM_5548.SRW",
        ] {
            let Some(data) = corpus_file(s) else { continue };
            assert!(region_matches_full(&data), "{s} should be row addressable");
        }
    }
}
