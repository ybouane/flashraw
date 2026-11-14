//! Leaf MOS: a TIFF with a lossless-JPEG or 16-bit raw image and the PKTS packet tree.

use super::OpenOptions;
use super::pkts::{self, Mosaic, Packets};
use crate::common::{
    Known, SimpleDecoder, apply_db, classify_previews, db_color, db_lookup, no_raw_ifd, push_jpeg, strips,
    tiff_previews, tiles,
};
use crate::medium::{ErrorSlot, decode_u16_rows, matrix_from_romm};
use flashraw_codecs::leaf::decode_linear;
use flashraw_codecs::ljpeg::LjpegHeader;
use flashraw_core::bytes::slice_at_lenient;
use flashraw_core::tiff::{Ifd, Tiff, tags};
use flashraw_core::*;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stream {
    Raw16,
    /// One lossless JPEG stream; `interleaved` rows are stored even rows first, odd rows backwards.
    Ljpeg {
        interleaved: bool,
    },
}

struct Plan<'a> {
    file: &'a [u8],
    width: u32,
    height: u32,
    /// Rows per strip or tile.
    band: usize,
    tiled: bool,
    units: Vec<(usize, usize)>,
    order: ByteOrder,
    stream: Stream,
    /// Height of the visible area (rows below it stay empty in interleaved captures).
    visible_rows: usize,
}

impl Plan<'_> {
    fn unit(&self, i: usize) -> DataUnit {
        if self.tiled { DataUnit::Tile(i as u32) } else { DataUnit::Strip(i as u32) }
    }

    fn decode(&self, ctx: &DecodeContext) -> Result<RawImage> {
        let mut img = RawImage::new_u16(self.width, self.height, 1, &ctx.limits)?;
        let (w, h) = (self.width as usize, self.height as usize);
        match self.stream {
            Stream::Raw16 => ctx.profiler.time(Stage::Unpack, || self.read_bands(ctx, img.u16_mut()))?,
            Stream::Ljpeg { interleaved } => {
                let &(offset, length) = self.units.first().ok_or_else(|| Error::corrupt("MOS: no tiles"))?;
                let unit = self.unit(0);
                let out = img.u16_mut();
                let stream_rows = |dest: &mut [u16]| -> Result<()> {
                    ctx.profiler
                        .time(Stage::EntropyDecode, || {
                            let src = slice_at_lenient(self.file, offset, length)?;
                            let hdr = LjpegHeader::parse(src, Default::default())?;
                            decode_linear(src, &hdr, dest, ctx.parallel)
                        })
                        .map_err(|e| e.or_truncated(self.file.len(), unit, offset, length))
                };
                if interleaved {
                    let mut scratch: Vec<u16> = alloc_zeroed(w * h)?;
                    stream_rows(&mut scratch)?;
                    let vis = self.visible_rows.min(h);
                    let even_rows = vis.div_ceil(2);
                    for r in 0..vis {
                        let j = if r < even_rows { 2 * r } else { 2 * (vis - 1 - r) + 1 };
                        out[r * w..(r + 1) * w].copy_from_slice(&scratch[j * w..(j + 1) * w]);
                    }
                } else {
                    stream_rows(out)?;
                }
            }
        }
        Ok(img)
    }

    /// Uncompressed rows: one unit holds everything, several hold `band` rows each.
    fn read_bands(&self, ctx: &DecodeContext, out: &mut [u16]) -> Result<()> {
        let w = self.width as usize;
        if let [(offset, _)] = self.units[..] {
            return decode_u16_rows(ctx, self.file, self.unit(0), offset, self.order, w, w * 2, out);
        }
        let errs = ErrorSlot::default();
        par::for_each_row_chunk(out, w, self.band, ctx.parallel, |first, chunk| {
            let i = first / self.band;
            match self.units.get(i) {
                None => errs.set(Error::corrupt(format!("MOS: missing band {i}"))),
                Some(&(offset, _)) => {
                    if let Err(e) = decode_u16_rows(ctx, self.file, self.unit(i), offset, self.order, w, w * 2, chunk) {
                        errs.set(e);
                    }
                }
            }
        });
        errs.into_result()
    }
}

/// The largest full-colour-depth image of the file.
fn raw_ifd<'t, 'a>(tiff: &'t Tiff<'a>) -> Option<&'t Ifd<'a>> {
    tiff.all_ifds()
        .into_iter()
        .filter(|i| i.bps() > 8 && i.width() > 0 && i.height() > 0 && !i.is_reduced_resolution())
        .max_by_key(|i| i.width() as u64 * i.height() as u64)
}

/// Display orientation from the accumulated rotation. A rotation that was never set (-1) defers to
/// the TIFF Orientation tag, whose Exif code is taken as it is; otherwise the rotation is a code or
/// an angle of the stored image.
fn orientation_from(rotation: i32, tiff_exif: impl FnOnce() -> u32) -> Orientation {
    let code = if !(-89..=89).contains(&rotation) {
        match rotation.wrapping_add(3600).rem_euclid(360) {
            270 => 5,
            180 => 3,
            90 => 6,
            _ => 0,
        }
    } else if rotation == -1 {
        return Orientation::from_exif(tiff_exif());
    } else {
        rotation as u32
    };
    Orientation::from_exif(match code {
        3 => 3,
        5 => 8,
        6 => 6,
        _ => 1,
    })
}

/// Visible rectangle of the backs whose frame includes dark margins.
fn visible_area(w: u32, h: u32) -> Option<Rect> {
    if w == 2116 {
        Some(Rect::new(55, 30, w.saturating_sub(110), h.saturating_sub(60)))
    } else if w == 3171 {
        Some(Rect::new(24, 24, w.saturating_sub(48), h.saturating_sub(48)))
    } else if w as u64 + h as u64 == 5204 {
        Some(if w > h { Rect::new(32, 6, 3072, 2048) } else { Rect::new(6, 32, 2048, 3072) })
    } else {
        None
    }
}

pub(super) fn open<'a>(data: &'a [u8], tiff: &Tiff<'a>, opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    let ifd0 = tiff.ifds.first().ok_or_else(|| Error::corrupt("MOS without IFD0"))?;
    let pkts_at = tiff.find_entry(super::TAG_LEAF_PKTS).map_or(data.len(), |e| e.value_pos);
    let packets: Packets = pkts::parse(data, pkts_at);

    let mut meta = RawMetadata::default();
    flashraw_core::exif::read_ifd0(ifd0, &mut meta);
    if !flashraw_camera_db::canonical_make(&meta.make).eq_ignore_ascii_case("Mamiya") {
        meta.make = "Leaf".into();
    }
    if let Some(m) = packets.back_model {
        meta.model = m.into();
    }
    if let Some(s) = &packets.serial {
        meta.serial = Some(s.clone());
    }
    if !packets.body.is_empty() {
        meta.set_extra("body", &packets.body);
    }
    let cam = db_lookup(&mut meta, "");

    let raw = raw_ifd(tiff).ok_or_else(|| no_raw_ifd(tiff, "MOS", Error::invalid_metadata("MOS: no raw image")))?;
    let samples = raw.u32(tags::SAMPLES_PER_PIXEL).unwrap_or(1);
    if samples > 1 {
        return Err(Error::unsupported_compression("MOS", format!("multi-shot capture ({samples} planes)")));
    }
    let (w, h) = (raw.width(), raw.height());
    opts.limits.check_dims(w, h, 1)?;
    let tiled = raw.get(tags::TILE_OFFSETS).is_some();
    let units = if tiled { tiles(raw) } else { strips(raw) };
    if units.is_empty() {
        return Err(Error::invalid_metadata("MOS: no data offsets"));
    }
    let band = raw.u32(tags::TILE_LENGTH).or(raw.u32(tags::ROWS_PER_STRIP)).unwrap_or(h).clamp(1, h.max(1)) as usize;
    if tiled && raw.u32(tags::TILE_WIDTH).is_some_and(|tw| tw != w) {
        return Err(Error::unsupported_compression("MOS", "tiles narrower than the image"));
    }
    let compression = raw.compression();
    let ljpeg = match compression {
        1 => false,
        7 | 99 => true,
        c => return Err(Error::unsupported_compression("MOS", format!("TIFF compression {c}"))),
    };
    if raw.bps() != 16 {
        return Err(Error::unsupported_compression("MOS", format!("{}-bit samples", raw.bps())));
    }

    let mut info = FrameInfo::new(w, h, 14);
    info.compression = if ljpeg { Compression::LeafLjpeg } else { Compression::Uncompressed };
    let wide = ljpeg
        && slice_at_lenient(data, units[0].0, units[0].1)
            .and_then(|s| LjpegHeader::parse(s, Default::default()))
            .is_ok_and(|hdr| hdr.bits == 15);
    info.white_level = WhiteLevel::uniform(if wide { 0x1fff } else { 0x3fff });

    let cfa = match packets.mosaic {
        Mosaic::Undecided | Mosaic::Phase(0) => CfaPattern::RGGB,
        Mosaic::Phase(1) => CfaPattern::GRBG,
        Mosaic::Phase(2) => CfaPattern::BGGR,
        Mosaic::Phase(_) => CfaPattern::GBRG,
        Mosaic::MultiPlane => {
            return Err(Error::unsupported_compression("MOS", format!("{}-plane capture", packets.planes)));
        }
    };
    info.cfa = Some(cfa);
    info.layout = SensorLayout::Bayer;

    if let Some([n0, n1, n2, n3]) = packets.neutrals.filter(|n| n[1] > 0 && n[2] > 0 && n[3] > 0) {
        let q = |d: i32| n0 as f32 / d as f32;
        info.white_balance = WhiteBalance::from_multipliers(q(n1), q(n2), q(n3), q(n2));
    }
    if db_color(&meta, cam).color.is_empty() {
        if let Some(m) = packets.matrix.and_then(|m| matrix_from_romm(&m)) {
            info.color_matrices = m;
        }
    }

    info.orientation =
        orientation_from(packets.rotation, || raw.u32(tags::ORIENTATION).or(ifd0.u32(tags::ORIENTATION)).unwrap_or(1));
    if let Some(area) = visible_area(w, h) {
        info.active_area = area;
        info.crop = area;
    }
    apply_db(
        &mut info,
        &meta,
        cam,
        Known { cfa: true, crop: true, active_area: true, black: true, white: true, masked: true, color: false },
    );
    meta.previews = tiff_previews(data, 0, tiff, info.orientation);
    if let Some((offset, size)) = packets.preview {
        push_jpeg(data, offset, size, info.orientation, &mut meta.previews);
        classify_previews(&mut meta.previews);
    }

    let plan = Arc::new(Plan {
        file: data,
        width: w,
        height: h,
        band,
        tiled,
        units,
        order: tiff.order,
        stream: if ljpeg { Stream::Ljpeg { interleaved: packets.rows_flags & 1 != 0 } } else { Stream::Raw16 },
        visible_rows: info.active_area.height as usize,
    });
    let random_access = match plan.stream {
        Stream::Ljpeg { .. } => RandomAccess::Sequential,
        Stream::Raw16 if band < h as usize => RandomAccess::Strips { rows: band as u32 },
        Stream::Raw16 => RandomAccess::Rows,
    };
    let shared = Arc::clone(&plan);
    let mut dec =
        SimpleDecoder::new(RawFormat::Mos, meta, info, Box::new(move |ctx: &DecodeContext| shared.decode(ctx)));
    dec.random_access = random_access;
    Ok(Box::new(dec))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotation_to_orientation() {
        let tiff = || 8;
        assert_eq!(orientation_from(90, tiff), Orientation::Rotate90);
        assert_eq!(orientation_from(270, tiff), Orientation::Rotate270);
        assert_eq!(orientation_from(180, tiff), Orientation::Rotate180);
        assert_eq!(orientation_from(0, tiff), Orientation::Normal);
        assert_eq!(orientation_from(45, tiff), Orientation::Normal);
        assert_eq!(orientation_from(-90, tiff), Orientation::Rotate270);
        assert_eq!(orientation_from(450, tiff), Orientation::Rotate90);
        // Small values are codes: 5 means a quarter turn the other way, 6 a quarter turn.
        assert_eq!(orientation_from(5, tiff), Orientation::Rotate270);
        assert_eq!(orientation_from(6, tiff), Orientation::Rotate90);
        assert_eq!(orientation_from(3, tiff), Orientation::Rotate180);
        // Never set: the TIFF tag decides, taken as an Exif value.
        assert_eq!(orientation_from(-1, tiff), Orientation::Rotate270);
        assert_eq!(orientation_from(-1, || 1), Orientation::Normal);
    }

    #[test]
    fn visible_areas_of_known_backs() {
        assert_eq!(visible_area(2116, 1000), Some(Rect::new(55, 30, 2006, 940)));
        assert_eq!(visible_area(3171, 2000), Some(Rect::new(24, 24, 3123, 1952)));
        assert_eq!(visible_area(3100, 2104), Some(Rect::new(32, 6, 3072, 2048)));
        assert_eq!(visible_area(2104, 3100), Some(Rect::new(6, 32, 2048, 3072)));
        assert_eq!(visible_area(4000, 3000), None);
    }
}
