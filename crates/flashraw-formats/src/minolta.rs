//! Minolta / Konica Minolta MRW and the Minolta RD-175 (`.MDC`).
//!
//! MRW starts with `\0MRM`, a 4-byte block length, then tagged blocks:
//!
//! | Block | Content |
//! |---|---|
//! | `PRD` | sensor size, bit depths, storage method (0x52 unpacked 16-bit, 0x59 packed 12-bit), Bayer pattern |
//! | `WBG` | as-shot white-balance gains in Bayer order |
//! | `RIF` | preset white balances, ISO, colour mode |
//! | `TTW` | an embedded TIFF with Make/Model/EXIF and the thumbnail |
//!
//! The image data follows the header blocks. Packed data is MSB-first 12-bit; the
//! DiMAGE Z2 stores 32-bit little-endian words and the odd rows in a second field.
//! The same block structure is embedded in Sony DSLR-A100 ARW files (via
//! DNGPrivateData), so [`parse_blocks`] is shared with the Sony module.
//!
//! The RD-175 (1995) has no header: it is identified by its exact size and stores
//! 8-bit samples of three CCDs interleaved into a GRBG mosaic.

use crate::common::*;
use flashraw_core::tiff::Tiff;
use flashraw_core::*;

/// `PRD` block contents.
#[derive(Debug, Clone, Copy, Default)]
pub struct Prd {
    pub raw_height: u16,
    pub raw_width: u16,
    pub image_height: u16,
    pub image_width: u16,
    pub total_bps: u8,
    pub active_bps: u8,
    pub storage: u8,
    pub bayer: u8,
}

const STORAGE_UNPACKED: u8 = 0x52;
const STORAGE_PACKED: u8 = 0x59;
const BAYER_G2BRG1: u8 = 0x04;
const RD175_SIZE: usize = 1_138_688;
const Z2_SIZE: usize = 5_869_568;

/// Parsed MRW header blocks.
#[derive(Debug, Clone, Default)]
pub struct MrwBlocks {
    pub order: Option<ByteOrder>,
    pub prd: Option<Prd>,
    /// As-shot multipliers in (R, G, B, G2) order.
    pub wb: Option<[f32; 4]>,
    /// Absolute offset and length of the embedded TIFF (`TTW`).
    pub ttw: Option<(usize, usize)>,
    /// Absolute offset of the image data (end of the header).
    pub data_offset: usize,
}

/// Parse the `\0MR?` block structure at `base`.
pub fn parse_blocks(data: &[u8], base: usize) -> Result<MrwBlocks> {
    let head = flashraw_core::bytes::slice_at(data, base, 8)?;
    if head[0] != 0 || head[1] != b'M' || head[2] != b'R' {
        return Err(Error::UnsupportedFormat("not an MRW block structure".into()));
    }
    let order = if head[3] == b'I' { ByteOrder::Little } else { ByteOrder::Big };
    let len = data.u32_at(order, base + 4)? as usize;
    let end = base.saturating_add(len).saturating_add(8).min(data.len().saturating_sub(8));
    let mut out =
        MrwBlocks { order: Some(order), data_offset: base.saturating_add(len).saturating_add(8), ..Default::default() };
    let mut pos = base + 8;
    let mut raw_wb: Option<[u16; 4]> = None;
    while pos < end {
        let tag = data.u32be(pos)?;
        let blen = data.u32_at(order, pos + 4)? as usize;
        let body = pos + 8;
        if body.checked_add(blen).is_none_or(|e| e > data.len()) {
            break;
        }
        match tag {
            0x0050_5244 => {
                // PRD
                let p = body + 8;
                out.prd = Some(Prd {
                    raw_height: data.u16_at(order, p)?,
                    raw_width: data.u16_at(order, p + 2)?,
                    image_height: data.u16_at(order, p + 4)?,
                    image_width: data.u16_at(order, p + 6)?,
                    total_bps: *data.get(p + 8).unwrap_or(&0),
                    active_bps: *data.get(p + 9).unwrap_or(&0),
                    storage: *data.get(p + 10).unwrap_or(&0),
                    bayer: *data.get(p + 15).unwrap_or(&0),
                });
            }
            0x0057_4247 => {
                // WBG
                let p = body + 4;
                raw_wb = Some([
                    data.u16_at(order, p)?,
                    data.u16_at(order, p + 2)?,
                    data.u16_at(order, p + 4)?,
                    data.u16_at(order, p + 6)?,
                ]);
            }
            0x0054_5457 => out.ttw = Some((body, blen)),
            _ => {}
        }
        pos = body + blen;
    }
    if let Some(v) = raw_wb {
        let v = v.map(|x| x as f32);
        // Stored in sensor Bayer order: G2 B R G1 or R G G B.
        out.wb = Some(if out.prd.is_some_and(|p| p.bayer == BAYER_G2BRG1) {
            [v[2], v[3], v[1], v[0]]
        } else {
            [v[0], v[1], v[3], v[2]]
        });
    }
    Ok(out)
}

/// MRW magic (`\0MRM`) or an RD-175 dump (identified by its exact size).
pub fn probe(data: &[u8]) -> bool {
    data.starts_with(b"\0MRM") || is_rd175(data) || is_z2(data)
}

/// DiMAGE Z2 "raw" files are headerless sensor dumps of a fixed size whose tail is
/// not zero-padded (unlike same-size dumps from other cameras).
fn is_z2(data: &[u8]) -> bool {
    data.len() == Z2_SIZE
        && !data.starts_with(b"II")
        && !data.starts_with(b"MM")
        && data[data.len() - 424..].iter().filter(|&&b| b != 0).count() > 20
}

/// Normalised model names (several regional names share one body).
fn normalized_model(model: &str) -> &str {
    const ALIASES: [(&str, &[&str]); 2] = [
        ("DG-5D", &["DYNAX 5D", "MAXXUM 5D", "ALPHA-5 DIGITAL", "ALPHA SWEET DIGITAL"]),
        ("DG-7D", &["DYNAX 7D", "MAXXUM 7D", "ALPHA-7 DIGITAL"]),
    ];
    for (canon, names) in ALIASES {
        if names.iter().any(|n| n.eq_ignore_ascii_case(model)) {
            return canon;
        }
    }
    model
}

fn is_rd175(data: &[u8]) -> bool {
    data.len() == RD175_SIZE && !data.starts_with(b"II") && !data.starts_with(b"MM") && !data.starts_with(b"\0MR")
}

pub fn open<'a>(data: &'a [u8], opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    if is_rd175(data) {
        return open_rd175(data, opts);
    }
    if is_z2(data) {
        return open_z2(data, opts);
    }
    let blocks = parse_blocks(data, 0)?;
    let prd = blocks.prd.ok_or_else(|| Error::invalid_metadata("MRW without PRD block"))?;
    let mut meta = RawMetadata::default();
    let mut orientation = Orientation::Normal;
    let mut tiff_holder = None;
    if let Some((o, l)) = blocks.ttw {
        if let Ok(t) = Tiff::parse(&data[o..o + l]) {
            for ifd in &t.ifds {
                flashraw_core::exif::read_ifd0(ifd, &mut meta);
            }
            orientation = meta.orientation;
            tiff_holder = Some((o, t));
        }
    }
    let (w, h) = (prd.raw_width as u32, prd.raw_height as u32);
    // PRD's image size is the camera's own output size (3008x2000 of 3016x2008, ...).
    meta.set_image_size(prd.image_width as u32, prd.image_height as u32);
    opts.limits.check_dims(w, h, 1)?;
    let packed = match prd.storage {
        STORAGE_PACKED => true,
        STORAGE_UNPACKED => false,
        s => return Err(Error::unsupported_compression("MRW", format!("storage method {s:#x}"))),
    };
    let bits = if prd.active_bps > 0 && prd.total_bps > 0 { prd.active_bps as u32 } else { 12 };
    if !(8..=16).contains(&bits) {
        return Err(Error::invalid_metadata(format!("MRW: {bits}-bit samples")));
    }
    let order = blocks.order.unwrap_or(ByteOrder::Big);
    let mode = if packed { "packed" } else { "unpacked" };
    let cam = db_lookup(&mut meta, mode);
    let _ = cam;

    let mut info = FrameInfo::new(w, h, bits as u8);
    info.compression = if packed { Compression::MinoltaPacked } else { Compression::Uncompressed };
    info.cfa = Some(if prd.bayer == BAYER_G2BRG1 { CfaPattern::GBRG } else { CfaPattern::RGGB });
    info.layout = SensorLayout::Bayer;
    let entry = crate::sony::table::lookup("Minolta", normalized_model(&meta.model));
    let (mut black, mut cblack, mut white) = (0, [0; 4], None);
    crate::sony::table::apply_levels(entry, &mut black, &mut cblack, &mut white);
    info.black_level = BlackLevel::uniform(black as f32);
    info.white_level = WhiteLevel::uniform(white.unwrap_or((1 << bits) - 1));
    info.white_balance = blocks.wb.and_then(|m| WhiteBalance::from_multipliers(m[0], m[1], m[2], m[3]));
    info.orientation = orientation;
    info.color_matrices = crate::sony::table::color(entry, &meta);
    if let Some((o, t)) = &tiff_holder {
        meta.previews = tiff_previews(data, *o, t, orientation);
        // The thumbnail lives in the Minolta makernote (a plain IFD whose offsets are
        // relative to the embedded TIFF): 0x81 holds the JPEG, 0x88/0x89 point to it.
        if let Some(mn) = t.find_entry(flashraw_core::tiff::tags::MAKER_NOTE) {
            let opts = flashraw_core::tiff::TiffOptions { follow_sub_ifds: false, ..Default::default() };
            if let Ok(ifd) = flashraw_core::tiff::parse_ifd_at(t.data, t.order, mn.value_pos as u32, opts) {
                if let Some(e) = ifd.get(0x0081).filter(|e| e.typ == 7) {
                    push_jpeg(data, o + e.value_pos, e.data.len(), orientation, &mut meta.previews);
                }
                if let (Some(po), Some(pl)) = (ifd.u32(0x0088), ifd.u32(0x0089)) {
                    push_jpeg(data, o + po as usize, pl as usize, orientation, &mut meta.previews);
                }
            }
        }
        classify_previews(&mut meta.previews);
    }

    let row_bytes = if packed { (w as usize * 12).div_ceil(8) } else { w as usize * 2 };
    // The payload runs from the end of the header; a payload past the end of the file is
    // reported when decoding (metadata and previews stay available).
    let data_offset = blocks.data_offset;
    let src = data.get(data_offset..).unwrap_or(&[]);
    let rows_needed = h as usize * row_bytes;
    let mut dec = SimpleDecoder::new(
        RawFormat::Mrw,
        meta,
        info,
        Box::new(move |ctx| {
            let geometry = RowGeometry::tight(h as usize, w as usize, if packed { 12 } else { 16 });
            let src = ctx.sensor_rows(data, DataUnit::Payload, data_offset, geometry)?;
            let mut img = RawImage::new_u16(w, h, 1, &ctx.limits)?;
            let pack_order = match (packed, order) {
                (true, _) => PackOrder::MsbBytes,
                (false, ByteOrder::Big) => PackOrder::MsbBytes,
                (false, ByteOrder::Little) => PackOrder::LsbBytes,
            };
            let layout = flashraw_codecs::PackedLayout {
                width: w as usize,
                height: h as usize,
                bits: if packed { 12 } else { 16 },
                order: pack_order,
                row_stride: row_bytes,
            };
            ctx.profiler.time(Stage::Unpack, || {
                flashraw_codecs::unpack(src, layout, img.u16_mut(), w as usize, ctx.parallel, ctx.simd, ctx.recover)
            })?;
            Ok(img)
        }),
    );
    dec.random_access = RandomAccess::Rows;
    if let Some(d) = src.get(..rows_needed) {
        dec.packed = Some(PackedPayload {
            data: d,
            width: w,
            height: h,
            bits: if packed { 12 } else { 16 },
            order: if packed || order == ByteOrder::Big { PackOrder::MsbBytes } else { PackOrder::LsbBytes },
            row_stride: row_bytes,
        });
    }
    Ok(Box::new(dec))
}

/// DiMAGE Z2: headerless 2288x1710 dump. 12-bit samples MSB-first inside 32-bit
/// little-endian words; the even rows come first, the odd rows start halfway
/// through the file.
fn open_z2<'a>(data: &'a [u8], opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    const W: u32 = 2288;
    const H: u32 = 1710;
    opts.limits.check_dims(W, H, 1)?;
    let mut meta = RawMetadata::new("Minolta", "DiMAGE Z2");
    db_lookup(&mut meta, "z2-headerless");
    let entry = crate::sony::table::lookup("Minolta", "DiMAGE Z2");
    let (mut black, mut cblack, mut white) = (0, [0; 4], None);
    crate::sony::table::apply_levels(entry, &mut black, &mut cblack, &mut white);
    let mut info = FrameInfo::new(W, H, 12);
    info.compression = Compression::MinoltaPacked;
    info.cfa = Some(CfaPattern::BGGR);
    info.black_level = BlackLevel::uniform(black as f32);
    info.white_level = WhiteLevel::uniform(white.unwrap_or(4095));
    info.color_matrices = crate::sony::table::color(entry, &meta);
    let dec = SimpleDecoder::new(RawFormat::Mrw, meta, info, Box::new(move |ctx| decode_z2(data, W, H, ctx)));
    Ok(Box::new(dec))
}

fn decode_z2(src: &[u8], w: u32, h: u32, ctx: &DecodeContext) -> Result<RawImage> {
    let mut img = RawImage::new_u16(w, h, 1, &ctx.limits)?;
    let (wu, hu) = (w as usize, h as usize);
    let half = hu.div_ceil(2);
    let second = (src.len() >> 3) << 2;
    if src.len() < wu * 12 / 8 * half {
        return Err(Error::corrupt("Z2: payload truncated"));
    }
    let out = img.u16_mut();
    ctx.profiler.time(Stage::Unpack, || {
        // Each field is one continuous 32-bit-word bit stream.
        for (field, start, rows) in [(0usize, 0usize, half), (1, second, hu - half)] {
            let mut p = flashraw_core::bits::BitPumpMsb32::new(&src[start..]);
            for i in 0..rows {
                let row = 2 * i + field;
                for v in out[row * wu..(row + 1) * wu].iter_mut() {
                    *v = flashraw_core::bits::BitRead::get(&mut p, 12) as u16;
                }
            }
        }
    });
    Ok(img)
}

/// Minolta RD-175: three 8-bit CCD readouts interleaved into a 1534x986 GRBG mosaic.
fn open_rd175<'a>(data: &'a [u8], opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    const W: u32 = 1534;
    const H: u32 = 986;
    const OFFSET: usize = 513;
    opts.limits.check_dims(W, H, 1)?;
    let mut meta = RawMetadata::new("Minolta", "RD175");
    db_lookup(&mut meta, "rd175");
    let mut info = FrameInfo::new(W, H, 9);
    info.compression = Compression::Uncompressed;
    info.cfa = Some(CfaPattern::GRBG);
    info.white_level = WhiteLevel::uniform(0x1fe);
    info.color_matrices = db_color(&meta, None);
    let src = &data[OFFSET..];
    let dec = SimpleDecoder::new(
        RawFormat::Other,
        meta,
        info,
        Box::new(move |ctx| {
            let mut img = RawImage::new_u16(W, H, 1, &ctx.limits)?;
            ctx.profiler.time(Stage::Unpack, || rd175_unpack(src, img.u16_mut()))?;
            Ok(img)
        }),
    );
    Ok(Box::new(dec))
}

/// Re-interleave the 1481 768-byte CCD lines of an RD-175 dump.
fn rd175_unpack(src: &[u8], out: &mut [u16]) -> Result<()> {
    const W: usize = 1534;
    for irow in 0..1481usize {
        let pixel = src.get(irow * 768..(irow + 1) * 768).ok_or_else(|| Error::corrupt("RD175: data truncated"))?;
        let mut bx = irow / 82;
        let mut row = irow % 82 * 12 + if bx < 12 { bx | 1 } else { (bx - 12) * 2 };
        match irow {
            1477 | 1479 => continue,
            1476 => row = 984,
            1480 => row = 985,
            1478 => {
                row = 985;
                bx = 1;
            }
            _ => {}
        }
        if row >= 986 {
            continue;
        }
        if bx < 12 && bx & 1 == 1 {
            // Green CCD lines carry both quincunx phases; interpolate the missing ones.
            let mut r = row;
            for col in 0..1533usize {
                if col != 1 && r < 986 {
                    out[r * W + col] = if (col + 1) & 2 != 0 {
                        let a = if col / 2 >= 1 { pixel[col / 2 - 1] as u16 } else { 0 };
                        a + pixel.get(col / 2 + 1).copied().unwrap_or(0) as u16
                    } else {
                        (pixel[col / 2] as u16) << 1
                    };
                }
                r ^= 1;
            }
            // The row toggled once per column, so these land on the partner row.
            if r < 986 {
                out[r * W + 1] = (pixel[1] as u16) << 1;
                out[r * W + 1533] = (pixel[765] as u16) << 1;
            }
        } else {
            for col in (row & 1..W).step_by(2) {
                out[row * W + col] = (pixel[col / 2] as u16) << 1;
            }
        }
    }
    Ok(())
}

pub fn open_tiff<'a>(data: &'a [u8], tiff: Tiff<'a>, opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    // Minolta-branded TIFF files are processed JPEG/TIFF images, except the Sony-built
    // DSLR-A100 heritage that is handled by the Sony module.
    let _ = (data, tiff, opts);
    Err(Error::UnsupportedFormat("Minolta TIFF without raw data (MRW files are detected by signature)".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mrw(prd_storage: u8, bayer: u8, w: u16, h: u16, payload: &[u8]) -> Vec<u8> {
        let mut blocks = vec![];
        // PRD
        blocks.extend(b"\0PRD");
        blocks.extend(24u32.to_be_bytes());
        blocks.extend(b"21810002");
        blocks.extend(h.to_be_bytes());
        blocks.extend(w.to_be_bytes());
        blocks.extend(h.to_be_bytes());
        blocks.extend(w.to_be_bytes());
        blocks.extend([if prd_storage == STORAGE_PACKED { 12 } else { 16 }, 12, prd_storage, 0, 0, 0, 0, bayer]);
        // WBG
        blocks.extend(b"\0WBG");
        blocks.extend(12u32.to_be_bytes());
        blocks.extend([0, 0, 0, 0]);
        for v in [400u16, 256, 256, 500] {
            blocks.extend(v.to_be_bytes());
        }
        let mut out = b"\0MRM".to_vec();
        out.extend((blocks.len() as u32).to_be_bytes());
        out.extend(blocks);
        out.extend_from_slice(payload);
        out
    }

    #[test]
    fn unpacked_and_packed_roundtrip() {
        let (w, h) = (24u16, 22u16);
        let vals: Vec<u16> = (0..w as usize * h as usize).map(|i| (i * 13 % 4096) as u16).collect();
        let payload: Vec<u8> = vals.iter().flat_map(|v| v.to_be_bytes()).collect();
        let f = mrw(STORAGE_UNPACKED, 1, w, h, &payload);
        assert!(probe(&f));
        let d = open(&f, &OpenOptions::default()).unwrap();
        let p = Profiler::new();
        let img = d.decode(&DecodeContext::new(&p)).unwrap();
        assert_eq!(img.u16(), &vals[..]);
        let wb = d.frame_info().white_balance.unwrap();
        assert!((wb.coeffs[0] - 400.0 / 256.0).abs() < 1e-6);
        assert!((wb.coeffs[2] - 500.0 / 256.0).abs() < 1e-6);

        let mut packed = vec![];
        for c in vals.chunks(2) {
            packed.extend([(c[0] >> 4) as u8, ((c[0] & 15) << 4 | c[1] >> 8) as u8, c[1] as u8]);
        }
        let f = mrw(STORAGE_PACKED, BAYER_G2BRG1, w, h, &packed);
        let d = open(&f, &OpenOptions::default()).unwrap();
        assert_eq!(d.frame_info().cfa, Some(CfaPattern::GBRG));
        let img = d.decode(&DecodeContext::new(&p)).unwrap();
        assert_eq!(img.u16(), &vals[..]);
        // Truncated payload: a structured error when strict, recovered with a warning
        // (complete samples kept, 100 bytes = 66 samples + a partial one zero) otherwise.
        let d = open(&f[..f.len() - 100], &OpenOptions::default()).unwrap();
        let mut strict = DecodeContext::new(&p);
        strict.recover = false;
        let t = d.decode(&strict).unwrap_err().truncation().cloned().unwrap();
        assert_eq!((t.expected - t.available, t.unit), (100, DataUnit::Payload));
        let ctx = DecodeContext::new(&p);
        let img = d.decode(&ctx).unwrap();
        let kept = vals.len() - 67;
        assert_eq!(&img.u16()[..kept], &vals[..kept]);
        assert!(img.u16()[kept..].iter().all(|&v| v == 0));
        assert_eq!(ctx.take_warnings()[0].samples_filled, 67);
    }

    #[test]
    fn garbage_headers_do_not_panic() {
        for n in 0..64 {
            let mut f = b"\0MRM".to_vec();
            f.extend((0..n).map(|i| (i * 37) as u8));
            let _ = open(&f, &OpenOptions::default()).map(|d| {
                let p = Profiler::new();
                d.decode(&DecodeContext::new(&p))
            });
        }
        let mut f = b"\0MRM\xff\xff\xff\xff\0PRD\xff\xff\xff\xff".to_vec();
        f.resize(64, 0);
        assert!(open(&f, &OpenOptions::default()).is_err());
    }
}
