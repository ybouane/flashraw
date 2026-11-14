//! Canon CR3: ISO BMFF container ([`container`]) with CRX-coded raw tracks.
//!
//! Opening a file parses the container, reads the EXIF directories of the Canon metadata box and the
//! maker notes of the timed-metadata track, derives the geometry of the sensor buffer and describes
//! the preview images. The raw track is decoded by the CRX codec when asked to.

pub mod container;

use super::geometry::{self, BandParams, Geometry};
use super::makernote::{CanonMakernote, black_pattern};
use crate::common::*;
use container::{Cr3, Media};
use flashraw_codecs::canon_crx;
use flashraw_core::exif;
use flashraw_core::tiff::{Ifd, Tiff, tags};
use flashraw_core::*;

/// True for an ISO BMFF file whose `ftyp` major brand is `crx `.
pub fn probe(data: &[u8]) -> bool {
    container::is_cr3(data)
}

/// The first IFD of an embedded TIFF stream (`CMT1`..`CMT4`, a maker-note record).
fn first_ifd<'t, 'a>(tiff: &'t Option<Tiff<'a>>) -> Option<&'t Ifd<'a>> {
    tiff.as_ref()?.ifds.first()
}

// ---------------------------------------------------------------------------------------------
// Timed metadata: maker notes carried next to the samples
// ---------------------------------------------------------------------------------------------

/// Record types of the timed-metadata sample that hold maker-note items.
const CTMD_MAKERNOTE_RECORDS: [u16; 2] = [7, 8];
/// Item tag of a Canon maker note (a complete TIFF stream).
const CTMD_MAKERNOTE_TAG: u32 = 0x927C;

fn le32(d: &[u8], at: usize) -> Option<usize> {
    Some(u32::from_le_bytes(d.get(at..at.checked_add(4)?)?.try_into().ok()?) as usize)
}

/// Bytes of sample 0 of a track, when they lie inside the file.
fn first_sample<'a>(data: &'a [u8], track: &container::Track) -> Option<(usize, &'a [u8])> {
    let (offset, size) = track.sample(0)?;
    let offset = usize::try_from(offset).ok()?;
    let end = offset.checked_add(usize::try_from(size).ok()?)?;
    Some((offset, data.get(offset..end)?))
}

/// The maker notes of every timed-metadata sample. A sample is a run of records (size, type, six more
/// header bytes, payload, little-endian); the maker-note records hold items of (length, tag, payload).
fn timed_notes(data: &[u8], cr3: &Cr3) -> Vec<CanonMakernote> {
    let mut notes = Vec::new();
    for track in cr3.tracks.iter().filter(|t| t.media == Media::TimedMetadata) {
        let Some((_, sample)) = first_sample(data, track) else { continue };
        let mut p = 0;
        while p + 6 <= sample.len() {
            let size = le32(sample, p).unwrap_or(0);
            let ty = u16::from_le_bytes([sample[p + 4], sample[p + 5]]);
            let Some(end) = p.checked_add(size).filter(|&e| size != 0 && e <= sample.len()) else { break };
            if CTMD_MAKERNOTE_RECORDS.contains(&ty) && size > 12 {
                collect_items(&sample[p + 12..end], &mut notes);
            }
            p = end;
        }
    }
    notes
}

fn collect_items(record: &[u8], notes: &mut Vec<CanonMakernote>) {
    let mut q = 0;
    while q + 9 <= record.len() {
        let (Some(len), Some(tag)) = (le32(record, q), le32(record, q + 4)) else { break };
        let Some(end) = q.checked_add(len).filter(|&e| len >= 8 && e <= record.len()) else { break };
        if tag as u32 == CTMD_MAKERNOTE_TAG {
            if let Some(ifd) = Tiff::parse(&record[q + 8..end]).ok().and_then(|t| t.ifds.into_iter().next()) {
                notes.push(CanonMakernote::parse(&ifd));
            }
        }
        q = end;
    }
}

/// The file's maker note: `CMT3`, completed by what the timed-metadata records add.
fn merged_note(cmt3: Option<&Ifd>, timed: Vec<CanonMakernote>) -> Option<CanonMakernote> {
    let mut mn = cmt3.map(CanonMakernote::parse);
    for n in timed {
        let mn = mn.get_or_insert_with(CanonMakernote::default);
        mn.color = mn.color.or(n.color);
        mn.sensor = mn.sensor.or(n.sensor);
        mn.aspect = mn.aspect.or(n.aspect);
        mn.multishot |= n.multishot;
    }
    mn
}

// ---------------------------------------------------------------------------------------------
// Previews
// ---------------------------------------------------------------------------------------------

/// `jpeg` as a slice of `data`: its offset in the file.
fn offset_in(data: &[u8], part: &[u8]) -> usize {
    (part.as_ptr() as usize).saturating_sub(data.as_ptr() as usize)
}

fn previews(data: &[u8], cr3: &Cr3, orientation: Orientation) -> Vec<PreviewInfo> {
    let mut out = Vec::new();
    for (w, h, jpeg) in cr3.thumbnail.into_iter().chain(cr3.preview) {
        if jpeg.len() < 4 || jpeg[0] != 0xFF || jpeg[1] != 0xD8 {
            continue;
        }
        let (pw, ph) = flashraw_codecs::jpeg::dimensions(jpeg).unwrap_or((w as u32, h as u32));
        let offset = offset_in(data, jpeg) as u64;
        out.push(PreviewInfo::new(
            offset,
            jpeg.len() as u64,
            pw,
            ph,
            PreviewEncoding::Jpeg,
            orientation,
            PreviewKind::Preview,
        ));
    }
    for track in &cr3.tracks {
        let (encoding, jpeg) = match track.media {
            Media::Jpeg => (PreviewEncoding::Jpeg, true),
            Media::Heif => (PreviewEncoding::Heif, false),
            _ => continue,
        };
        let Some((offset, bytes)) = first_sample(data, track) else { continue };
        let dims = if jpeg {
            flashraw_codecs::jpeg::dimensions(bytes)
        } else {
            Some((track.entry_width as u32, track.entry_height as u32))
        };
        let Some((w, h)) = dims else { continue };
        out.push(PreviewInfo::new(
            offset as u64,
            bytes.len() as u64,
            w,
            h,
            encoding,
            orientation,
            PreviewKind::Preview,
        ));
    }
    classify_previews(&mut out);
    out
}

// ---------------------------------------------------------------------------------------------
// Open
// ---------------------------------------------------------------------------------------------

/// The colour filter pattern at the buffer origin as written in the CRX header
/// (0 RGGB, 1 GRBG, 2 GBRG, 3 BGGR; anything else is taken as RGGB).
fn header_pattern(layout: u8) -> &'static str {
    match layout {
        1 => "GRBG",
        2 => "GBRG",
        3 => "BGGR",
        _ => "RGGB",
    }
}

/// Open a CR3 file.
pub fn open<'a>(data: &'a [u8], opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    let cr3 = Cr3::parse(data)?;
    let (_, track, hdr) = cr3.main_raw().ok_or_else(|| Error::unsupported_compression("CR3", "no CRX raw track"))?;
    let (offset, size) = track.sample(0).ok_or_else(|| Error::corrupt("CR3 raw track without samples"))?;
    let offset = usize::try_from(offset).unwrap_or(usize::MAX);
    let size = usize::try_from(size).unwrap_or(usize::MAX);
    if size < hdr.mdat_header_size as usize {
        return Err(Error::corrupt("CR3 raw sample smaller than its header"));
    }
    opts.limits.check_dims(hdr.width, hdr.height, 1)?;
    let (raw_w, raw_h) = (hdr.width, hdr.height);

    // EXIF directories of the Canon metadata box.
    let tiffs: [Option<Tiff<'a>>; 4] = std::array::from_fn(|i| cr3.cmt[i].and_then(|d| Tiff::parse(d).ok()));
    let mut meta = RawMetadata::default();
    let ifd0 = first_ifd(&tiffs[0]);
    if let Some(i) = ifd0 {
        exif::read_ifd0(i, &mut meta);
    }
    if let Some(i) = first_ifd(&tiffs[1]) {
        exif::read_exif(i, &mut meta);
    }
    if let Some(i) = first_ifd(&tiffs[3]) {
        meta.gps = exif::read_gps(i);
    }
    if meta.make.is_empty() {
        meta.make = "Canon".into();
    }
    let cam = db_lookup(&mut meta, if hdr.levels > 0 { "craw" } else { "raw" });
    meta.set_extra("crx_version", format!("0x{:x}", hdr.version));
    meta.set_extra("crx_levels", hdr.levels);
    if cr3.raw_count() > 1 {
        meta.set_extra("raw_images", cr3.raw_count());
    }

    let bits = if hdr.enc_type == 3 { hdr.median_bits } else { hdr.bits };
    let mut info = FrameInfo::new(raw_w, raw_h, bits);
    info.compression = Compression::CanonCrx;
    info.layout = SensorLayout::Bayer;
    info.white_level = WhiteLevel::uniform(hdr.white_level());
    info.orientation = ifd0.and_then(|i| i.u32(tags::ORIENTATION)).map(Orientation::from_exif).unwrap_or_default();

    // Maker note: lens, firmware and white balance.
    let mn = merged_note(first_ifd(&tiffs[2]), timed_notes(data, &cr3));
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
        info.white_balance = mn.as_shot_wb().and_then(|[r, g, b, g2]| WhiteBalance::from_multipliers(r, g, b, g2));
    }
    let known = apply_geometry(&mut info, hdr.cfa_layout, mn.as_ref());

    if let Some(c) = cam {
        // Rebadged models share their twin's matrix.
        let mut primary = meta.clone();
        primary.canonical_model = c.canonical_model.to_string();
        info.color_matrices = db_color(&primary, cam);
    }
    apply_db(&mut info, &meta, cam, known);
    if let Some(r) =
        mn.as_ref().and_then(|mn| mn.image_area(raw_w, raw_h, false)).and_then(|r| r.intersect(&info.active_area))
    {
        info.crop = even_crop_origin(r, info.active_area);
    }
    meta.previews = previews(data, &cr3, info.orientation);

    let hdr = hdr.clone();
    let decode = move |ctx: &DecodeContext| -> Result<RawImage> {
        let file_len = data.len();
        if offset >= file_len {
            return Err(Error::truncated(Truncation::new(
                DataUnit::Payload,
                offset as u64,
                size as u64,
                file_len as u64,
            )));
        }
        let mut img = RawImage::new_u16(hdr.width, hdr.height, 1, &ctx.limits)?;
        ctx.profiler
            .time(Stage::EntropyDecode, || canon_crx::decode(&hdr, &data[offset..], img.u16_mut(), ctx.parallel))
            .map_err(|e| e.or_truncated(file_len, DataUnit::Payload, offset, size))?;
        Ok(img)
    };
    Ok(Box::new(SimpleDecoder::new(RawFormat::Cr3, meta, info, Box::new(decode))))
}

/// Active area, colour filter phase and black level of the CRX buffer; returns what the file settled.
///
/// Rows of the geometry table apply by buffer size; buffers without a row use the maker note's
/// SensorInfo and per-channel black level when ColorData has them.
fn apply_geometry(info: &mut FrameInfo, cfa_layout: u8, mn: Option<&CanonMakernote>) -> Known {
    let (raw_w, raw_h) = (info.width, info.height);
    let mut known = Known { cfa: true, white: true, ..Known::default() };
    let row = geometry::find(raw_w, raw_h);
    let mut area = row.map_or(Geometry { left: 0, top: 0, width: raw_w, height: raw_h }, |r| r.geometry());
    let band = row.map_or(BandParams { left_start: 2, left_guard: 2, right_gap: 2, right_trim: 0 }, |r| r.band);
    let visible = row
        .and_then(|r| r.forced_cfa)
        .or(Some(header_pattern(cfa_layout)))
        .and_then(CfaPattern::parse)
        .unwrap_or(CfaPattern::RGGB);

    let channel_black = if row.is_none() {
        mn.and_then(|m| m.color).filter(|c| c.average_black() > 0).and_then(|c| c.black)
    } else {
        None
    };
    if channel_black.is_some() {
        if let Some(s) = mn.and_then(|m| m.sensor).filter(|s| (s.width as u32, s.height as u32) == (raw_w, raw_h)) {
            let left = (s.crop.left as u32 + 1) & !1;
            let top = (s.crop.top as u32 + 1) & !1;
            area = Geometry {
                left,
                top,
                width: (s.crop.right as u32 + 1).saturating_sub(left),
                height: (s.crop.bottom as u32 + 1).saturating_sub(top),
            };
        }
    }

    // The pattern at the buffer origin follows from the margins before the even rounding.
    let (sx, sy) = (((2 - area.left % 2) % 2) as usize, ((2 - area.top % 2) % 2) as usize);
    info.cfa = Some(visible.shifted(sx, sy));
    if area.top % 2 == 1 {
        area.top += 1;
        area.height = area.height.saturating_sub(1);
    }
    if area.left % 2 == 1 {
        area.left += 1;
        area.width = area.width.saturating_sub(1);
    }
    if row.is_some()
        || channel_black.is_some()
        || (area.left, area.top, area.width, area.height) != (0, 0, raw_w, raw_h)
    {
        let active = Rect::new(area.left, area.top, area.width, area.height).clamp_to(raw_w, raw_h);
        info.active_area = active;
        info.crop = active;
        known.active_area = true;
        known.crop = true;
    }
    if let Some(black) = channel_black {
        info.black_level = black_pattern(black, &visible, sx, sy);
        known.black = true;
    } else if row.is_some() {
        let masked: Vec<Rect> = geometry::masked_columns(&area, raw_w, &band)
            .into_iter()
            .filter(|&(x0, x1)| x1 > x0 && area.height > 0)
            .map(|(x0, x1)| Rect::new(x0, area.top, x1 - x0, area.height))
            .collect();
        if !masked.is_empty() {
            info.masked_areas = masked;
            info.black_from_masked = true;
            known.black = true;
            known.masked = true;
        }
    }
    known
}
