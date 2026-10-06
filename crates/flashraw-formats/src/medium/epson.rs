//! Epson ERF (R-D1, R-D1s, R-D1x).
//!
//! TIFF with the raw image in a SubIFD: TIFF compression 32769, 12-bit samples packed
//! MSB-first, ten samples in fifteen bytes followed by one padding byte (16-byte
//! groups). The Epson maker note (`EPSON\0` header, IFD in the file byte order with
//! file-relative offsets) carries the per-channel black levels (tag 0x0401, RGGB
//! order) and the white balance (tag 0x0e80).

use crate::common::*;
use flashraw_core::tiff::{Ifd, Tiff, tags};
use flashraw_core::*;

/// Unpack one row of Epson 12-bit data: groups of 16 bytes = 10 samples + 1 pad byte.
pub fn unpack_row_12_padded(src: &[u8], out: &mut [u16]) {
    for (g, o) in src.chunks(16).zip(out.chunks_mut(10)) {
        let mut acc = 0u32;
        let mut nbits = 0u32;
        let mut it = g.iter().take(15);
        for v in o.iter_mut() {
            while nbits < 12 {
                acc = (acc << 8) | *it.next().unwrap_or(&0) as u32;
                nbits += 8;
            }
            nbits -= 12;
            *v = ((acc >> nbits) & 0xFFF) as u16;
        }
    }
}

/// Bytes per row of padded 12-bit Epson data.
fn row_bytes(width: usize) -> usize {
    width.div_ceil(10) * 16
}

/// Bytes holding a padded row's `width` samples (the final group's padding is not needed).
fn padded_row_need(width: usize) -> usize {
    match width % 10 {
        0 => (width / 10 * 16).saturating_sub(1),
        r => width / 10 * 16 + (r * 12).div_ceil(8),
    }
}

/// Samples of a padded row entirely within its first `len` bytes.
fn padded_complete(len: usize) -> usize {
    len / 16 * 10 + (len % 16).min(15) * 8 / 12
}

struct Maker {
    black: Option<[u32; 4]>,
    wb: Option<(f32, f32)>,
}

fn parse_makernote(data: &[u8], tiff: &Tiff) -> Maker {
    let mut m = Maker { black: None, wb: None };
    let Some(e) = tiff.find_entry(tags::MAKER_NOTE) else { return m };
    let pos = e.value_pos;
    if data.get(pos..pos + 6) != Some(b"EPSON\0") {
        return m;
    }
    let Ok(ifd) = flashraw_core::tiff::parse_ifd_at(
        data,
        tiff.order,
        (pos + 8) as u32,
        flashraw_core::tiff::TiffOptions { follow_sub_ifds: false, ..Default::default() },
    ) else {
        return m;
    };
    if let Some(b) = ifd.get(0x0401) {
        let v = b.u32s();
        if v.len() >= 4 {
            m.black = Some([v[0], v[1], v[2], v[3]]);
        }
    }
    if let Some(w) = ifd.get(0x0e80) {
        let b = w.bytes();
        if let (Ok(r), Ok(bl)) = (b.u16_at(tiff.order, 48), b.u16_at(tiff.order, 50)) {
            m.wb = Some((r as f32 * 567.0 / 65536.0, bl as f32 * 431.0 / 65536.0));
        }
    }
    m
}

fn raw_ifd<'t, 'a>(tiff: &'t Tiff<'a>) -> Option<&'t Ifd<'a>> {
    tiff.all_ifds()
        .into_iter()
        .filter(|i| i.u32(tags::PHOTOMETRIC) == Some(32803) && i.width() > 0 && i.height() > 0)
        .max_by_key(|i| i.width() as u64 * i.height() as u64)
}

/// Open an Epson ERF.
pub fn open_tiff<'a>(data: &'a [u8], tiff: Tiff<'a>, opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    let ifd0 = tiff.ifds.first().ok_or_else(|| Error::corrupt("ERF without IFD0"))?;
    let mut meta = RawMetadata::default();
    flashraw_core::exif::read_ifd0(ifd0, &mut meta);
    let cam = db_lookup(&mut meta, "");
    let raw = raw_ifd(&tiff).ok_or_else(|| no_raw_ifd(&tiff, "ERF", Error::invalid_metadata("ERF: no CFA image")))?;
    let (w, h) = (raw.width(), raw.height());
    opts.limits.check_dims(w, h, 1)?;
    let comp = raw.compression();
    let bps = raw.bps();
    let padded = match (comp, bps) {
        (32769, 12) => true,
        (1, 12) => false,
        (c, b) => return Err(Error::unsupported_compression("ERF", format!("compression {c}, {b} bits"))),
    };
    let (off, len) = *strips(raw).first().ok_or_else(|| Error::invalid_metadata("ERF: no strips"))?;
    let rb = if padded { row_bytes(w as usize) } else { (w as usize * 12).div_ceil(8) };
    let need = if padded { padded_row_need(w as usize) } else { rb };
    let _ = len; // addressed by geometry: the declared length does not matter

    let mut info = FrameInfo::new(w, h, 12);
    info.compression = Compression::Uncompressed;
    let cfa = tiff_ep_cfa(raw).unwrap_or(CfaPattern::RGGB);
    info.cfa = Some(cfa);
    info.layout = layout_for(&cfa);
    info.orientation = Orientation::from_exif(ifd0.u32(tags::ORIENTATION).unwrap_or(1));
    let mn = parse_makernote(data, &tiff);
    let mut known = Known { cfa: true, crop: true, active_area: true, white: true, masked: true, ..Default::default() };
    if let Some(b) = mn.black {
        // Values are R, G (red row), G (blue row), B; place them on the CFA.
        let mut v = [0f32; 4];
        for y in 0..2 {
            for x in 0..2 {
                let other_g_on_red_row = (0..2).any(|xx| cfa.color_at(xx, y) == CfaColor::Red);
                v[y * 2 + x] = match cfa.color_at(x, y) {
                    CfaColor::Red => b[0],
                    CfaColor::Blue => b[3],
                    _ if other_g_on_red_row => b[1],
                    _ => b[2],
                } as f32;
            }
        }
        info.black_level = BlackLevel::pattern2x2(v);
        known.black = true;
    }
    if let Some((r, b)) = mn.wb {
        info.white_balance = WhiteBalance::from_multipliers(r, 1.0, b, 1.0);
    }
    apply_db(&mut info, &meta, cam, known);
    meta.previews = tiff_previews(data, 0, &tiff, info.orientation);
    let (wu, hu) = (w as usize, h as usize);
    let decode = move |ctx: &DecodeContext| -> Result<RawImage> {
        let mut img = RawImage::new_u16(w, h, 1, &ctx.limits)?;
        let geometry = RowGeometry { rows: hu, stride: rb, row_bytes: need, samples: wu };
        let src = ctx.sensor_bytes(data, DataUnit::Strip(0), off, geometry.needed())?;
        let (truncation, src) = (src.truncation, src.bytes);
        let filled = std::sync::atomic::AtomicU64::new(0);
        ctx.profiler.time(Stage::Unpack, || {
            par::for_each_row_chunk_bounded(
                img.u16_mut(),
                wu,
                par::memory_bound_workers(),
                ctx.parallel,
                |first, chunk| {
                    for (i, row) in chunk.chunks_mut(wu).enumerate() {
                        let y = first + i;
                        let s = src.get(y * rb..).unwrap_or(&[]);
                        let s = &s[..rb.min(s.len())];
                        // Samples not entirely within the data (recovering) stay zero.
                        let n = if padded { padded_complete(s.len()) } else { s.len() * 8 / 12 }.min(wu);
                        if n < wu {
                            filled.fetch_add((wu - n) as u64, std::sync::atomic::Ordering::Relaxed);
                        }
                        if n == 0 {
                            continue;
                        }
                        if padded {
                            unpack_row_12_padded(s, &mut row[..n]);
                        } else {
                            flashraw_codecs::packed::unpack_row(s, &mut row[..n], 12, PackOrder::MsbBytes, ctx.simd);
                        }
                    }
                },
            );
        });
        if let Some(t) = truncation {
            ctx.recovered(CorruptReason::Truncated(t), filled.into_inner());
        }
        Ok(img)
    };
    let mut dec = SimpleDecoder::new(RawFormat::Erf, meta, info, Box::new(decode));
    dec.random_access = RandomAccess::Rows;
    Ok(Box::new(dec))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn padded_row_needs() {
        assert_eq!(padded_row_need(20), 31);
        assert_eq!(padded_row_need(12), 16 + 3);
        assert_eq!(padded_row_need(13), 16 + 5);
        assert_eq!(padded_complete(31), 20);
        assert_eq!(padded_complete(30), 19);
        assert_eq!(padded_complete(16 + 3), 12);
    }

    #[test]
    fn padded_groups() {
        // Ten 12-bit samples MSB-packed in 15 bytes + 1 pad, twice.
        let samples: Vec<u16> = (0..20).map(|i| (i * 211 + 7) as u16 & 0xFFF).collect();
        let mut bytes = Vec::new();
        for g in samples.chunks(10) {
            let mut acc = 0u128;
            for &s in g {
                acc = (acc << 12) | s as u128;
            }
            for k in (0..15).rev() {
                bytes.push((acc >> (8 * k)) as u8);
            }
            bytes.push(0xEE);
        }
        let mut out = vec![0u16; 20];
        unpack_row_12_padded(&bytes, &mut out);
        assert_eq!(out, samples);
        // Truncated input does not panic.
        unpack_row_12_padded(&bytes[..7], &mut out);
    }
}
