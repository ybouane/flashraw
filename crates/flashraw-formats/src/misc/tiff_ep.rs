//! Standards-based fallback for TIFF/EP-style CFA raws from makes without a
//! dedicated module: the largest full-resolution IFD with PhotometricInterpretation
//! 32803 (CFA), uncompressed (MSB-first packed rows, or 16-bit words in the file's
//! byte order) or lossless JPEG, CFA layout from the TIFF/EP `CFARepeatPatternDim`/
//! `CFAPattern` tags (or the EXIF `CFAPattern`), white level from `BitsPerSample`.
//! Anything the standard tags do not describe is left to the camera database.

use crate::common::*;
use flashraw_codecs::ljpeg;
use flashraw_core::tiff::{Ifd, Tiff, tags};
use flashraw_core::*;

fn find_cfa_ifd<'t, 'a>(tiff: &'t Tiff<'a>) -> Option<&'t Ifd<'a>> {
    tiff.all_ifds()
        .into_iter()
        .filter(|i| i.u32(tags::PHOTOMETRIC) == Some(32803))
        .filter(|i| !i.is_reduced_resolution())
        .filter(|i| i.width() > 0 && i.height() > 0 && matches!(i.compression(), 1 | 7))
        .max_by_key(|i| i.width() as u64 * i.height() as u64)
}

pub fn open<'a>(data: &'a [u8], tiff: Tiff<'a>, opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    let ifd0 = tiff.ifds.first().ok_or_else(|| Error::corrupt("TIFF without IFD0"))?;
    let mut meta = RawMetadata::default();
    flashraw_core::exif::read_ifd0(ifd0, &mut meta);
    let raw = find_cfa_ifd(&tiff).ok_or_else(|| {
        Error::UnsupportedFormat(format!("TIFF from {:?} without a TIFF/EP CFA image", meta.make.trim()))
    })?;
    let (w, h) = (raw.width(), raw.height());
    opts.limits.check_dims(w, h, 1)?;
    if raw.u32(tags::SAMPLES_PER_PIXEL).unwrap_or(1) != 1 {
        return Err(Error::unsupported_compression("TIFF/EP", "CFA image with several samples per pixel"));
    }
    let bps = raw.bps();
    if !(8..=16).contains(&bps) {
        return Err(Error::unsupported_compression("TIFF/EP", format!("{bps}-bit CFA data")));
    }
    let cfa = tiff_ep_cfa(raw)
        .or_else(|| tiff.all_ifds().into_iter().find_map(|i| i.exif.as_ref().and_then(|e| tiff_ep_cfa(e))))
        .ok_or_else(|| Error::invalid_metadata("TIFF/EP CFA image without a CFA pattern"))?;
    let cam = db_lookup(&mut meta, "");
    let mut info = FrameInfo::new(w, h, bps as u8);
    info.cfa = Some(cfa);
    info.layout = layout_for(&cfa);
    info.orientation = meta.orientation;
    let compression = raw.compression();
    info.compression = if compression == 7 { Compression::LosslessJpeg } else { Compression::Uncompressed };
    apply_db(&mut info, &meta, cam, Known { cfa: true, ..Default::default() });
    meta.previews = tiff_previews(data, 0, &tiff, info.orientation);

    let order = tiff.order;
    let units = strips(raw);
    if units.is_empty() {
        return Err(Error::invalid_metadata("TIFF/EP CFA image without strips"));
    }
    let rows_per_strip = raw.u32(tags::ROWS_PER_STRIP).unwrap_or(h).clamp(1, h) as usize;
    let payload = contiguous_payload(data, raw)?;
    let contiguous = units.windows(2).all(|p| p[0].0 + p[0].1 == p[1].0);
    let (wu, hu) = (w as usize, h as usize);
    let decode = move |ctx: &DecodeContext| -> Result<RawImage> {
        let mut img = RawImage::new_u16(w, h, 1, &ctx.limits)?;
        let out = img.u16_mut();
        if compression == 7 {
            // One lossless-JPEG stream per strip, each covering `rows_per_strip` rows.
            ctx.profiler.time(Stage::EntropyDecode, || -> Result<()> {
                for (s, &range) in units.iter().enumerate() {
                    let base = (s * rows_per_strip * wu).min(out.len());
                    let end = (base + rows_per_strip * wu).min(out.len());
                    let out = std::cell::RefCell::new(&mut *out);
                    let decode = |src: &[u8]| -> Result<()> {
                        let mut out = out.borrow_mut();
                        let hdr = ljpeg::LjpegHeader::parse(src, ljpeg::LjpegOptions::default())?;
                        let mut pos = 0usize;
                        ljpeg::decode_rows_par(src, &hdr, ctx.parallel, |_, v| {
                            let end = (base + pos + v.len()).min(out.len());
                            if base + pos < end {
                                out[base + pos..end].copy_from_slice(&v[..end - base - pos]);
                            }
                            pos += v.len();
                            Ok(())
                        })
                    };
                    let clear = || out.borrow_mut()[base..end].fill(0);
                    let unit = DataUnit::Strip(s as u32);
                    compressed_unit(data, ctx, unit, range, (end - base) as u64, decode, clear)?;
                }
                Ok(())
            })?;
        } else {
            let porder =
                if bps == 16 && order == ByteOrder::Little { PackOrder::LsbBytes } else { PackOrder::MsbBytes };
            let layout = flashraw_codecs::PackedLayout::tight(wu, hu, bps, porder);
            ctx.profiler.time(Stage::Unpack, || -> Result<()> {
                if contiguous {
                    let src = payload.rows(data, ctx, RowGeometry::tight(hu, wu, bps))?;
                    flashraw_codecs::unpack(src, layout, out, wu, ctx.parallel, ctx.simd, ctx.recover)
                } else {
                    for (s, &(o, _)) in units.iter().enumerate() {
                        let y0 = s * rows_per_strip;
                        let rows = rows_per_strip.min(hu.saturating_sub(y0));
                        if rows == 0 {
                            break;
                        }
                        let g = RowGeometry::tight(rows, wu, bps);
                        let src = ctx.sensor_rows(data, DataUnit::Strip(s as u32), o, g)?;
                        let l2 = flashraw_codecs::PackedLayout::tight(wu, rows, bps, porder);
                        flashraw_codecs::unpack(src, l2, &mut out[y0 * wu..], wu, false, ctx.simd, ctx.recover)?;
                    }
                    Ok(())
                }
            })?;
        }
        Ok(img)
    };
    let mut dec = SimpleDecoder::new(RawFormat::Other, meta, info, Box::new(decode));
    dec.random_access = if compression == 1 { RandomAccess::Rows } else { RandomAccess::Sequential };
    Ok(Box::new(dec))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal little-endian TIFF: entries `(tag, type, count, value bytes)`, then `payload`.
    fn tiff_file(entries: &[(u16, u16, u32, Vec<u8>)], payload: &[u8]) -> Vec<u8> {
        let mut e = entries.to_vec();
        e.sort_by_key(|x| x.0);
        let n = e.len();
        let mut extra_off = 8 + 2 + n * 12 + 4;
        let mut extra: Vec<u8> = Vec::new();
        let mut out = b"II*\0\x08\0\0\0".to_vec();
        out.extend((n as u16).to_le_bytes());
        for (tag, typ, count, data) in &e {
            out.extend(tag.to_le_bytes());
            out.extend(typ.to_le_bytes());
            out.extend(count.to_le_bytes());
            if data.len() <= 4 {
                let mut d = data.clone();
                d.resize(4, 0);
                out.extend(d);
            } else {
                out.extend((extra_off as u32).to_le_bytes());
                extra.extend(data);
                extra_off += data.len();
            }
        }
        out.extend(0u32.to_le_bytes());
        out.extend(extra);
        out.extend(payload);
        out
    }

    fn entries(w: u32, h: u32, data_off: u32, with_pattern: bool) -> Vec<(u16, u16, u32, Vec<u8>)> {
        let s = |v: u16| v.to_le_bytes().to_vec();
        let l = |v: u32| v.to_le_bytes().to_vec();
        let mut e = vec![
            (tags::MAKE, 2, 6, b"Nobody".to_vec()),
            (tags::IMAGE_WIDTH, 4, 1, l(w)),
            (tags::IMAGE_LENGTH, 4, 1, l(h)),
            (tags::BITS_PER_SAMPLE, 3, 1, s(16)),
            (tags::COMPRESSION, 3, 1, s(1)),
            (tags::PHOTOMETRIC, 3, 1, s(32803)),
            (tags::STRIP_OFFSETS, 4, 1, l(data_off)),
            (tags::STRIP_BYTE_COUNTS, 4, 1, l(w * h * 2)),
            (tags::CFA_REPEAT_PATTERN_DIM, 3, 2, [s(2), s(2)].concat()),
        ];
        if with_pattern {
            e.push((tags::CFA_PATTERN_EP, 1, 4, vec![1, 0, 2, 1]));
        }
        e
    }

    #[test]
    fn uncompressed_16bit_cfa() {
        let (w, h) = (4u32, 2u32);
        let payload: Vec<u8> = (0..(w * h) as u16).flat_map(|i| (i * 100).to_le_bytes()).collect();
        let probe_len = tiff_file(&entries(w, h, 0, true), &[]).len() as u32;
        let file = tiff_file(&entries(w, h, probe_len, true), &payload);
        let tiff = Tiff::parse(&file).unwrap();
        let dec = open(&file, tiff, &OpenOptions::default()).unwrap();
        assert_eq!(dec.frame_info().cfa, Some(CfaPattern::GRBG));
        let p = Profiler::default();
        let img = dec.decode(&DecodeContext::new(&p)).unwrap();
        assert_eq!(img.u16()[5], 500);
        // Without a CFA pattern the image is rejected rather than guessed.
        let file = tiff_file(&entries(w, h, probe_len - 12, false), &payload);
        let tiff = Tiff::parse(&file).unwrap();
        assert!(open(&file, tiff, &OpenOptions::default()).is_err());
    }

    fn decode_with(file: &[u8], recover: bool) -> (Result<RawImage>, Vec<DecodeWarning>) {
        let dec = open(file, Tiff::parse(file).unwrap(), &OpenOptions::default()).expect("metadata opens");
        let p = Profiler::default();
        let mut ctx = DecodeContext::new(&p);
        ctx.recover = recover;
        let img = dec.decode(&ctx);
        (img, ctx.take_warnings())
    }

    #[test]
    fn truncated_payload_fails_or_recovers() {
        let (w, h) = (4u32, 2u32);
        let payload: Vec<u8> = (1..=(w * h) as u16).flat_map(|i| (i * 100).to_le_bytes()).collect();
        let probe_len = tiff_file(&entries(w, h, 0, true), &[]).len() as u32;
        // Last row cut by 3 bytes: the first 6 samples survive.
        let file = tiff_file(&entries(w, h, probe_len, true), &payload[..payload.len() - 3]);
        let (img, _) = decode_with(&file, false);
        let t = img.unwrap_err().truncation().cloned().expect("structured truncation");
        assert_eq!((t.unit, t.expected, t.available), (DataUnit::Strip(0), 16, 13));
        let (img, warnings) = decode_with(&file, true);
        assert_eq!(img.unwrap().u16(), &[100, 200, 300, 400, 500, 600, 0, 0]);
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].samples_filled, 2);
    }

    #[test]
    fn strip_offset_past_eof_with_zero_byte_count() {
        // Regression (fuzzing): a zero byte count and an offset past the end of the file
        // must not underflow when the payload range is resolved.
        let (w, h) = (4u32, 2u32);
        let mut e = entries(w, h, 1_000_000, true);
        for x in e.iter_mut().filter(|x| x.0 == tags::STRIP_BYTE_COUNTS) {
            x.3 = 0u32.to_le_bytes().to_vec();
        }
        let file = tiff_file(&e, &[]);
        let (img, _) = decode_with(&file, false);
        assert!(img.unwrap_err().truncation().is_some_and(|t| t.starts_beyond_eof()));
        let (img, warnings) = decode_with(&file, true);
        assert!(img.unwrap().u16().iter().all(|&v| v == 0));
        assert_eq!(warnings[0].samples_filled, 8);
    }
}
