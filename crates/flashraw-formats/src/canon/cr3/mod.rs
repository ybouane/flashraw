//! Canon CR3: ISO BMFF container ([`container`]) with CRX-coded raw tracks
//! ([`flashraw_codecs::canon_crx`]).
//!
//! The decoded track is the primary image: the first raw track with the largest
//! `bits * width * height` (the first image of dual-pixel files), first sample (the
//! first frame of raw bursts / roll files).

pub mod container;

use super::geometry::{self, Geometry};
use super::makernote::CanonMakernote;
use crate::common::*;
use container::{Cr3, Media};
use flashraw_codecs::canon_crx::{self, CrxHeader};
use flashraw_core::tiff::{Tiff, tags};
use flashraw_core::*;

pub fn probe(data: &[u8]) -> bool {
    container::is_cr3(data)
}

pub fn open<'a>(data: &'a [u8], opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    let cr3 = Cr3::parse(data)?;
    let (_, track, hdr) = cr3.main_raw().ok_or_else(|| Error::unsupported_compression("CR3", "no CRX raw track"))?;
    let hdr = hdr.clone();
    let (offset, size) = track.sample(0).ok_or_else(|| Error::corrupt("CR3 raw track without samples"))?;
    // A sample past the end of the file fails when decoded (metadata and previews stay
    // available).
    let offset = usize::try_from(offset).unwrap_or(usize::MAX);
    let size = usize::try_from(size).unwrap_or(usize::MAX);
    if size < hdr.mdat_header_size as usize {
        return Err(Error::corrupt("CR3 raw sample smaller than its header"));
    }
    opts.limits.check_dims(hdr.width, hdr.height, 1)?;

    let mut meta = RawMetadata::default();
    let tiffs: Vec<Option<Tiff>> = cr3.cmt.iter().map(|c| c.and_then(|d| Tiff::parse(d).ok())).collect();
    let ifd0 = tiffs[0].as_ref().and_then(|t| t.ifds.first());
    if let Some(i) = ifd0 {
        flashraw_core::exif::read_ifd0(i, &mut meta);
    }
    if let Some(e) = tiffs[1].as_ref().and_then(|t| t.ifds.first()) {
        flashraw_core::exif::read_exif(e, &mut meta);
    }
    if let Some(g) = tiffs[3].as_ref().and_then(|t| t.ifds.first()) {
        meta.gps = flashraw_core::exif::read_gps(g);
    }
    if meta.make.is_empty() {
        meta.make = "Canon".into();
    }
    let mode = if hdr.levels > 0 { "craw" } else { "raw" };
    let cam = db_lookup(&mut meta, mode);
    meta.set_extra("crx_version", format!("0x{:x}", hdr.version));
    meta.set_extra("crx_levels", hdr.levels);
    let raws = cr3.raw_count();
    if raws > 1 {
        meta.set_extra("raw_images", raws);
    }

    let bits = if hdr.enc_type == 3 { hdr.median_bits } else { hdr.bits };
    let mut info = FrameInfo::new(hdr.width, hdr.height, bits);
    info.compression = Compression::CanonCrx;
    info.cfa = Some(match hdr.cfa_layout {
        1 => CfaPattern::parse("GRBG"),
        2 => CfaPattern::parse("GBRG"),
        3 => CfaPattern::parse("BGGR"),
        _ => CfaPattern::parse("RGGB"),
    })
    .flatten();
    info.layout = SensorLayout::Bayer;
    info.white_level = WhiteLevel::uniform(hdr.white_level());
    info.orientation = ifd0.and_then(|i| i.u32(tags::ORIENTATION)).map(Orientation::from_exif).unwrap_or_default();
    let mut mn = tiffs[2].as_ref().and_then(|t| t.ifds.first()).map(CanonMakernote::parse);
    // CR3 keeps the colour data (white balance, black levels) in maker notes inside
    // the timed-metadata (CTMD) track rather than in CMT3.
    for extra in ctmd_makernotes(data, &cr3) {
        let m = mn.get_or_insert_with(CanonMakernote::default);
        if m.color.is_none() {
            m.color = extra.color;
        }
        if m.sensor.is_none() {
            m.sensor = extra.sensor;
        }
        m.multishot |= extra.multishot;
    }
    if let Some(mn) = &mn {
        if meta.lens.model.is_none() {
            meta.lens.model = mn.lens_model.clone();
        }
        if meta.lens.focal_range.is_none() {
            meta.lens.focal_range = mn.focal_range;
        }
        if meta.firmware.is_none() {
            meta.firmware = mn.firmware.clone();
        }
        if let Some([r, g, b, g2]) = mn.as_shot_wb() {
            info.white_balance = WhiteBalance::from_multipliers(r, g, b, g2);
        }
    }
    let known = apply_geometry(&mut info, mn.as_ref());
    // Colour matrices are keyed by the primary body name (aliases such as "Rebel SL3"
    // or "Kiss M" share their twin's matrix).
    if let Some(c) = cam {
        let mut primary = meta.clone();
        primary.canonical_model = c.canonical_model.to_string();
        info.color_matrices = db_color(&primary, cam);
    }
    apply_db(&mut info, &meta, cam, known);

    meta.previews = previews(data, &cr3, info.orientation);

    let decode = move |ctx: &DecodeContext| -> Result<RawImage> { decode_sample(data, offset, size, &hdr, ctx) };
    Ok(Box::new(SimpleDecoder::new(RawFormat::Cr3, meta, info, Box::new(decode))))
}

/// Visible area, CFA and black level for CRX files: the
/// sensor table by raw size, else (with a maker-note black level) the maker-note
/// crop when it describes this buffer. Black is measured from the optical-black
/// columns when the table geometry applies, else taken from the maker note.
fn apply_geometry(info: &mut FrameInfo, mn: Option<&CanonMakernote>) -> Known {
    let (raw_w, raw_h) = (info.width, info.height);
    let mut known = Known { cfa: true, white: true, ..Default::default() };
    let Some(mut vis) = info.cfa else { return known };
    let mut g = Geometry { left: 0, top: 0, width: raw_w, height: raw_h };
    let mut insets = [0u32; 4];
    let layout = geometry::find(raw_w, raw_h);
    if let Some(l) = layout {
        g = l.geometry();
        insets = [l.mask_left_start, l.mask_left_end_inset, l.mask_right_start, l.mask_right_end_inset];
        // Table entries with an explicit CFA override the codec header's layout.
        let pat = match l.filters {
            0x94 => Some("RGGB"),
            0x61 => Some("GRBG"),
            0x49 => Some("GBRG"),
            0x16 => Some("BGGR"),
            _ => None,
        };
        if let Some(p) = pat.and_then(CfaPattern::parse) {
            vis = p;
        }
    }
    let mut channel_black = None;
    let color = mn.and_then(|m| m.color.as_ref());
    if layout.is_none() && color.is_some_and(|c| c.average_black() > 0) {
        channel_black = color.and_then(|c| c.black);
        if let Some(s) = mn.and_then(|m| m.sensor).filter(|s| s.width as u32 == raw_w && s.height as u32 == raw_h) {
            let left = (s.crop.left as u32 + 1) & !1;
            let top = (s.crop.top as u32 + 1) & !1;
            g = Geometry {
                left,
                top,
                width: (s.crop.right as u32).saturating_sub(left),
                height: (s.crop.bottom as u32).saturating_sub(top),
            };
        }
    }
    // The CFA (`vis`, from the codec header) is relative to the visible origin;
    // odd margins are then evened out.
    let (sx, sy) = ((2 - g.left as usize % 2) % 2, (2 - g.top as usize % 2) % 2);
    let buf_cfa = vis.shifted(sx, sy);
    if g.top % 2 == 1 {
        g.top += 1;
        g.height = g.height.saturating_sub(1);
    }
    if g.left % 2 == 1 {
        g.left += 1;
        g.width = g.width.saturating_sub(1);
    }
    info.cfa = Some(buf_cfa);
    if layout.is_some() || channel_black.is_some() || g.width != raw_w || g.height != raw_h {
        info.active_area = Rect::new(g.left, g.top, g.width, g.height).clamp_to(raw_w, raw_h);
        info.crop = info.active_area;
        known.active_area = true;
        known.crop = true;
    }
    if let Some(b) = channel_black {
        // File order R, G1, G2, B → visible CFA positions, re-anchored at (0, 0).
        let mut vals = [0f32; 4];
        let mut greens = [b[1], b[2]].into_iter();
        for (i, v) in vals.iter_mut().enumerate() {
            *v = match vis.color_at(i % 2, i / 2) {
                CfaColor::Red => b[0],
                CfaColor::Blue => b[3],
                _ => greens.next().unwrap_or(b[1]),
            } as f32;
        }
        info.black_level = BlackLevel::pattern2x2(vals).shifted(sx, sy);
        known.black = true;
    } else if layout.is_some() {
        for (x0, x1) in geometry::masked_columns(&g, raw_w, insets) {
            if x1 > x0 && g.height > 0 {
                info.masked_areas.push(Rect::new(x0, g.top, x1 - x0, g.height));
            }
        }
        if !info.masked_areas.is_empty() {
            info.black_from_masked = true;
            known.black = true;
            known.masked = true;
        }
    }
    known
}

/// Maker notes embedded in the first CTMD sample: records of `u32 size, u16 type`;
/// types 7 and 8 hold `(u32 length, u32 tag)` items after a 12-byte header, where tag
/// 0x927C is a TIFF-wrapped Canon maker note.
fn ctmd_makernotes(data: &[u8], cr3: &Cr3) -> Vec<CanonMakernote> {
    let mut out = Vec::new();
    for t in cr3.tracks.iter().filter(|t| t.media == Media::TimedMetadata) {
        let Some((off, len)) = t.sample(0) else { continue };
        let (Ok(off), Ok(len)) = (usize::try_from(off), usize::try_from(len)) else { continue };
        let Some(s) = off.checked_add(len).filter(|&e| e <= data.len()).map(|e| &data[off..e]) else { continue };
        let mut pos = 0usize;
        while pos + 6 < s.len() {
            let size = u32::from_le_bytes([s[pos], s[pos + 1], s[pos + 2], s[pos + 3]]) as usize;
            let typ = u16::from_le_bytes([s[pos + 4], s[pos + 5]]);
            if size < 1 || pos + size > s.len() {
                break;
            }
            if typ == 7 || typ == 8 {
                let rec = &s[pos..pos + size];
                let mut p = 12usize;
                while p + 8 < rec.len() {
                    let l = u32::from_le_bytes([rec[p], rec[p + 1], rec[p + 2], rec[p + 3]]) as usize;
                    let tag = u32::from_le_bytes([rec[p + 4], rec[p + 5], rec[p + 6], rec[p + 7]]);
                    if l < 8 || p + l > rec.len() {
                        break;
                    }
                    if tag == 0x927C {
                        if let Ok(tiff) = Tiff::parse(&rec[p + 8..p + l]) {
                            if let Some(ifd) = tiff.ifds.first() {
                                out.push(CanonMakernote::parse(ifd));
                            }
                        }
                    }
                    p += l;
                }
            }
            pos += size;
        }
    }
    out
}

/// Decode the CRX sample of `size` bytes at `offset`. One entropy-coded stream: it fails
/// under either corruption policy; a failure over a range past the end of the file is
/// reported as truncation.
fn decode_sample(data: &[u8], offset: usize, size: usize, hdr: &CrxHeader, ctx: &DecodeContext) -> Result<RawImage> {
    let unit = DataUnit::Payload;
    if offset >= data.len() {
        return Err(Error::truncated(Truncation::new(unit, offset as u64, size as u64, data.len() as u64)));
    }
    let mut img = RawImage::new_u16(hdr.width, hdr.height, 1, &ctx.limits)?;
    let sample = &data[offset..];
    ctx.profiler
        .time(Stage::EntropyDecode, || canon_crx::decode(hdr, sample, img.u16_mut(), ctx.parallel))
        .map_err(|e| e.or_truncated(data.len(), unit, offset, size))?;
    Ok(img)
}

/// THMB thumbnail, PRVW preview and the full-size JPEG / HEIF tracks.
fn previews(data: &[u8], cr3: &Cr3, orientation: Orientation) -> Vec<PreviewInfo> {
    let mut out = Vec::new();
    let base = data.as_ptr() as usize;
    let offset_of = |s: &[u8]| (s.as_ptr() as usize - base) as u64;
    for (w, h, jpeg) in [cr3.thumbnail, cr3.preview].into_iter().flatten() {
        let (jw, jh) = flashraw_codecs::jpeg::dimensions(jpeg).unwrap_or((w as u32, h as u32));
        if jpeg.len() >= 4 && jpeg[0] == 0xFF && jpeg[1] == 0xD8 {
            out.push(PreviewInfo::new(
                offset_of(jpeg),
                jpeg.len() as u64,
                jw,
                jh,
                PreviewEncoding::Jpeg,
                orientation,
                PreviewKind::Preview,
            ));
        }
    }
    for t in &cr3.tracks {
        let encoding = match t.media {
            Media::Jpeg => PreviewEncoding::Jpeg,
            Media::Heif => PreviewEncoding::Heif,
            _ => continue,
        };
        let Some((off, len)) = t.sample(0) else { continue };
        let (Ok(o), Ok(l)) = (usize::try_from(off), usize::try_from(len)) else { continue };
        let Some(s) = o.checked_add(l).filter(|&e| e <= data.len()).map(|e| &data[o..e]) else { continue };
        let (w, h) = match encoding {
            PreviewEncoding::Jpeg => match flashraw_codecs::jpeg::dimensions(s) {
                Some(d) => d,
                None => continue,
            },
            _ => (t.entry_width as u32, t.entry_height as u32),
        };
        out.push(PreviewInfo::new(off, len, w, h, encoding, orientation, PreviewKind::Preview));
    }
    classify_previews(&mut out);
    out
}

#[cfg(test)]
mod tests;
