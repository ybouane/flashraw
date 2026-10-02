//! Canon CR3: ISO BMFF container ([`container`]) with CRX-coded raw tracks
//! ([`flashraw_codecs::canon_crx`]).
//!
//! `bits * width * height` (the first image of dual-pixel files), first sample (the
//! first frame of raw bursts / roll files).

pub mod container;

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
    let offset = usize::try_from(offset)
        .ok()
        .filter(|&o| o < data.len())
        .ok_or_else(|| Error::corrupt("CR3 raw sample offset beyond end of file"))?;
    if size < hdr.mdat_header_size as u64 {
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
    apply_db(&mut info, &meta, cam, Known { cfa: true, white: true, ..Default::default() });

    meta.previews = previews(data, &cr3, info.orientation);

    let decode = move |ctx: &DecodeContext| -> Result<RawImage> { decode_sample(data, offset, &hdr, ctx) };
    Ok(Box::new(SimpleDecoder::new(RawFormat::Cr3, meta, info, Box::new(decode))))
}

fn decode_sample(data: &[u8], offset: usize, hdr: &CrxHeader, ctx: &DecodeContext) -> Result<RawImage> {
    let mut img = RawImage::new_u16(hdr.width, hdr.height, 1, &ctx.limits)?;
    let sample = &data[offset..];
    ctx.profiler.time(Stage::EntropyDecode, || canon_crx::decode(hdr, sample, img.u16_mut(), ctx.parallel))?;
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
            out.push(PreviewInfo {
                offset: offset_of(jpeg),
                length: jpeg.len() as u64,
                width: jw,
                height: jh,
                encoding: PreviewEncoding::Jpeg,
                orientation,
                kind: PreviewKind::Preview,
            });
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
        out.push(PreviewInfo {
            offset: off,
            length: len,
            width: w,
            height: h,
            encoding,
            orientation,
            kind: PreviewKind::Preview,
        });
    }
    classify_previews(&mut out);
    out
}

#[cfg(test)]
mod tests;
