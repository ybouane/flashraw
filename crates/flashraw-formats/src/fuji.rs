//! Fujifilm RAF container reader.
//!
//! A RAF file holds a fixed header, an embedded JPEG preview with EXIF and the Fujifilm
//! maker note, a big-endian record directory, and a sensor-data section: a small TIFF on
//! cameras since about 2010, bare samples on older ones. The sensor samples are plain
//! 16-bit words, bit-packed rows, vertical tiles (one medium-format back) or the
//! Fujifilm compressed stream.

mod container;
mod geometry;
mod payload;
#[cfg(test)]
mod tests;

use crate::common::{self, Known, OpenOptions, SimpleDecoder};
use container::{Directory, Embedded, Header, TiffSection};
use flashraw_codecs::fuji_compressed::{ColorTile, FujiCompressed, FujiHeader};
use flashraw_core::tiff::Tiff;
use flashraw_core::*;
use payload::{Source, Store};
use std::sync::Arc;

/// True when `data` starts with the RAF signature.
pub fn probe(data: &[u8]) -> bool {
    data.starts_with(container::SIGNATURE)
}

/// Entry point for TIFF files whose Make is Fujifilm but which are not RAF containers.
pub fn open_tiff<'a>(_data: &'a [u8], _tiff: Tiff<'a>, _opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    Err(Error::UnsupportedFormat("Fujifilm TIFF without RAF container".into()))
}

/// Open a RAF file: parse container and metadata and return a decoder for the sensor data.
pub fn open<'a>(data: &'a [u8], opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    let hdr = Header::parse(data)?;
    let mut meta = RawMetadata::default();
    let embedded = container::read_embedded(data, &hdr, &mut meta);
    if meta.make.is_empty() {
        meta.make = "FUJIFILM".to_string();
    }
    if meta.model.is_empty() {
        meta.model = hdr.model.clone();
    }
    let model = normalised_model(&meta.model);
    let dir = Directory::read(data, hdr.directory_offset);

    let stored = Stored::locate(data, &hdr, &dir, &embedded, &model)?;
    opts.limits.check_dims(stored.width, stored.height, 1)?;
    if stored.width < 22 || stored.height < 22 {
        return Err(Error::invalid_metadata(format!("RAF raw size {}x{} too small", stored.width, stored.height)));
    }

    let mode = if matches!(stored.store, Store::Compressed(_)) { "golomb-rice" } else { "" };
    let cam = common::db_lookup(&mut meta, mode);
    meta.set_extra("raf_version", &hdr.version);
    if embedded.crop_mode != 0 {
        meta.set_extra("crop_mode", embedded.crop_mode);
    }
    if dir.wide {
        meta.set_extra("fuji_sensor_rotated", if dir.layout { "layout1" } else { "layout0" });
    }

    // Geometry and pattern.
    let area = geometry::active_area(&geometry::Inputs {
        model: &model,
        nominal: (i64::from(stored.nominal.0), i64::from(stored.nominal.1)),
        stored: (i64::from(stored.width), i64::from(stored.height)),
        image_size: dir.image_size.map(|(h, w)| (i64::from(w), i64::from(h))),
        generation: dir.size_blob.generation,
        size_table: dir.size_blob.size.map(|(w, h)| (i64::from(w), i64::from(h))),
        crop_mode: embedded.crop_mode,
        vendor_crop: dir
            .crop_origin
            .zip(dir.crop_size)
            .map(|((t, l), (h, w))| (i64::from(l), i64::from(t), i64::from(w), i64::from(h))),
        layout: dir.layout,
        wide: dir.wide,
        xtrans: dir.xtrans.is_some(),
        packed: stored.packed_flag,
    });
    let mut info = FrameInfo::new(stored.width, stored.height, stored.bits.min(16) as u8);
    info.active_area = area.rect(stored.width, stored.height);
    info.crop = match dir.crop_origin.zip(dir.crop_size) {
        Some(((t, l), (h, w))) if !dir.wide => {
            Rect::new(l, t, w, h).intersect(&info.active_area).unwrap_or(info.active_area)
        }
        _ => info.active_area,
    };
    info.compression = if matches!(stored.store, Store::Compressed(_)) {
        Compression::FujiCompressed
    } else {
        Compression::Uncompressed
    };
    info.orientation = meta.orientation;
    if (dir.size_blob.generation == 0 && stored.nominal.0 == 2944) || stored.width == 5504 {
        // The S2 Pro and the medium-format back are mounted rotated.
        info.orientation = Orientation::Rotate90;
        meta.orientation_info = meta.orientation_info.camera_override(Orientation::Rotate90);
    }
    let (cfa, layout) =
        geometry::pattern(dir.xtrans.as_ref(), &area, dir.layout, info.active_area).ok_or_else(|| {
            Error::invalid_metadata(if area.superccd { "invalid SuperCCD geometry" } else { "invalid X-Trans layout" })
        })?;
    info.cfa = Some(cfa);
    info.layout = layout;

    // SuperCCD bodies saturate below the 14-bit maximum (15866 to 15896 in the samples), at a
    // level the file does not state: the camera database's measured white applies to them.
    let mut known = Known { cfa: true, crop: true, active_area: true, white: !area.superccd, ..Known::default() };

    // Black level.
    let from_tiff = stored.tiff.as_ref().map(|t| &t.black).filter(|b| !b.is_empty());
    if let Some(list) = from_tiff {
        let dim = (1..8).take_while(|d| (d + 1) * (d + 1) <= list.len()).last().map_or(1, |d| d + 1);
        info.black_level = BlackLevel::pattern(dim, dim, list[..dim * dim].iter().map(|&v| v as f32).collect());
        known.black = true;
    } else if let Some(b) = dir.black {
        info.black_level = BlackLevel::pattern2x2(black_on_pattern(&cfa, b));
        known.black = true;
    }

    // White level.
    let bps = stored.bits;
    let white = if bps >= 16 {
        65535
    } else if area.fourteen_bit {
        16383
    } else {
        (1u32 << bps) - 1
    };
    info.white_level = WhiteLevel::uniform(white);

    // Fallback calibration by model prefix.
    let fallback = flashraw_camera_db::MATRICES
        .iter()
        .find(|t| t.canonical_make.eq_ignore_ascii_case("Fujifilm") && starts_with_ignore_case(&model, t.model_prefix));
    if let Some(t) = fallback {
        if t.black > 0 {
            info.black_level = BlackLevel::uniform(t.black as f32);
        } else if t.black < 0 && info.black_level.values.iter().all(|&v| v == 0.0) {
            info.black_level = BlackLevel::uniform(-t.black as f32);
        }
        if t.white > 0 {
            info.white_level = WhiteLevel::uniform(t.white as u32);
        }
        if t.color_matrix.len() >= 9 {
            let values: Vec<f32> = t.color_matrix[..9].iter().map(|&v| v as f32 / 10000.0).collect();
            if let Some(m) = ColorMatrix::new(Illuminant::D65, 3, values) {
                info.color_matrices =
                    ColorMatrices { color: vec![m], source: ColorSource::CameraDb, ..Default::default() };
                known.color = true;
            }
        }
    }
    if stored.tiff.is_none() && info.white_level.max() > 4095 {
        // Bare-sample files do not state their depth; the SuperCCD bodies span 14 bits.
        info.bits_per_sample = 14;
    }

    // As-shot white balance.
    let rgb = stored
        .tiff
        .as_ref()
        .and_then(|t| t.white_balance)
        .or_else(|| dir.white_balance.map(|w| corrected_balance(&model, w)));
    info.white_balance =
        rgb.and_then(|[r, g, b]| WhiteBalance::from_multipliers(r as f32, g as f32, b as f32, g as f32));

    common::apply_db(&mut info, &meta, cam, known);

    let mut previews = Vec::new();
    common::push_jpeg(data, hdr.jpeg_offset, hdr.jpeg_len, info.orientation, &mut previews);
    common::classify_previews(&mut previews);
    meta.previews = previews;

    Ok(stored.into_decoder(data, meta, info, &cfa, &dir))
}

/// `data[offset..offset + len]` as far as the file holds it; an empty slice at the end of
/// the file when `offset` is beyond it.
fn clamped(data: &[u8], offset: usize, len: usize) -> &[u8] {
    let start = offset.min(data.len());
    &data[start..offset.saturating_add(len).min(data.len()).max(start)]
}

/// The model without a leading `FUJIFILM` and `FinePix`.
fn normalised_model(model: &str) -> String {
    let m = model.trim();
    let m = m.strip_prefix("FUJIFILM").unwrap_or(m).trim();
    m.strip_prefix("FinePix").unwrap_or(m).trim().to_string()
}

fn starts_with_ignore_case(text: &str, prefix: &str) -> bool {
    text.as_bytes().get(..prefix.len()).is_some_and(|t| t.eq_ignore_ascii_case(prefix.as_bytes()))
}

/// Early bodies store scaled red and blue numbers; these factors bring them to the
/// scale of green.
fn corrected_balance(model: &str, [r, g, b]: [u32; 3]) -> [u32; 3] {
    let (rc, bc) = if ["S2Pro", "S20Pro", "F700", "S5000", "S7000"].iter().any(|n| model.contains(n)) {
        ((10.0 / 17.0) / 0.652941, (2.0 / 3.0) / (3.0 / 4.0 + 1.0 / 300.0))
    } else if model.contains("DBP") || model.contains("DX-2000") {
        (0.7632653061, 0.8591549296)
    } else {
        return [r, g, b];
    };
    // Truncated toward zero, as the cameras' own scale is integral.
    [(f64::from(r) * rc) as i32 as u32, g, (f64::from(b) * bc) as i32 as u32]
}

/// Lay the four recorded black levels `(R, G, B, G2)` on the top-left 2x2 of `cfa`.
fn black_on_pattern(cfa: &CfaPattern, [r, g, b, g2]: [f32; 4]) -> [f32; 4] {
    let mut greens = 0;
    let mut out = [0.0; 4];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = match cfa.color_at(i % 2, i / 2) {
            CfaColor::Red => r,
            CfaColor::Blue => b,
            _ => {
                greens += 1;
                if greens == 1 { g } else { g2 }
            }
        };
    }
    out
}

/// Colour index (0 red, 1 green, 2 blue) of each element of a 6x6 tile.
fn color_tile(xtrans: Option<&[u8; 36]>, cfa: &CfaPattern) -> ColorTile {
    let mut tile = [[1u8; 6]; 6];
    for (row, line) in tile.iter_mut().enumerate() {
        for (col, slot) in line.iter_mut().enumerate() {
            *slot = match xtrans {
                Some(p) => p[row * 6 + col],
                None => match cfa.color_at(col, row) {
                    CfaColor::Red => 0,
                    CfaColor::Blue => 2,
                    _ => 1,
                },
            };
        }
    }
    tile
}

/// The sensor section resolved: where the payload is, how it is stored, and the final
/// size of the stored buffer.
struct Stored<'a> {
    tiff: Option<TiffSection>,
    /// Sensor size as the container states it (before the compressed header replaces it).
    nominal: (u32, u32),
    width: u32,
    height: u32,
    /// Bits per sample as recorded.
    bits: u32,
    /// The TIFF flagged the samples as bit-packed 12-bit (or fewer) rows.
    packed_flag: bool,
    payload_offset: usize,
    declared: usize,
    payload: &'a [u8],
    store: Store<'a>,
}

impl<'a> Stored<'a> {
    fn locate(data: &'a [u8], hdr: &Header, dir: &Directory, embedded: &Embedded, model: &str) -> Result<Stored<'a>> {
        let section = data.get(hdr.section_offset..).unwrap_or(&[]);
        match TiffSection::parse(section) {
            Some(t) => Stored::from_tiff(data, hdr, t, model),
            None => Stored::from_bare(data, hdr, dir, embedded, model),
        }
    }

    fn from_tiff(data: &'a [u8], hdr: &Header, t: TiffSection, model: &str) -> Result<Stored<'a>> {
        let payload_offset = hdr
            .section_offset
            .checked_add(t.payload_offset)
            .ok_or_else(|| Error::corrupt("RAF payload offset overflows"))?;
        let payload = clamped(data, payload_offset, t.declared);
        let (w, h, bps, declared) = (t.width, t.height, t.bits, t.declared);
        let samples = u64::from(w) * u64::from(h);
        let compressed = |payload: &'a [u8]| FujiHeader::parse(payload).is_ok().then(|| FujiCompressed::new(payload));
        let mut packed_flag = false;
        let store = if let (Some(msb), true) = (t.packing, bps <= 12) {
            packed_flag = true;
            Store::packed(w, bps, if msb { PackOrder::Msb32 } else { PackOrder::LsbBytes })
        } else if samples * 2 != declared as u64 {
            if samples * 7 / 4 == declared as u64 {
                Store::packed(w, 14, PackOrder::Msb32)
            } else if let Some(c) = compressed(payload) {
                Store::Compressed(c?)
            } else {
                Store::words(ByteOrder::Little, w, w)
            }
        } else if model.eq_ignore_ascii_case("X-H2S") && data.len() < payload_offset.saturating_add(declared) {
            // This body records the uncompressed size even for compressed data.
            match compressed(payload) {
                Some(c) => Store::Compressed(c?),
                None => Store::words(ByteOrder::Little, w, w),
            }
        } else {
            Store::words(ByteOrder::Little, w, w)
        };
        let (width, height) = match &store {
            Store::Compressed(c) => (c.header.width, c.header.height),
            _ => (w, h),
        };
        Ok(Stored {
            nominal: (w, h),
            width,
            height,
            bits: bps,
            packed_flag,
            payload_offset,
            declared,
            payload,
            store,
            tiff: Some(t),
        })
    }

    fn from_bare(
        data: &'a [u8],
        hdr: &Header,
        dir: &Directory,
        embedded: &Embedded,
        model: &str,
    ) -> Result<Stored<'a>> {
        let (h, mut w) = dir.raw_size.ok_or_else(|| Error::invalid_metadata("RAF without raw size record"))?;
        let order = embedded.order.unwrap_or(ByteOrder::Little);
        let payload = clamped(data, hdr.section_offset, usize::MAX);
        if dir.layout && hdr.second_frame {
            // Both exposures sit side by side in each stored row.
            w *= 2;
        }
        let (width, store) = if w == 5504 {
            (w, Store::Tiled { tiles: 8, order })
        } else if model.starts_with("S20Pro") || model.starts_with("F700") {
            // Only the first of the two interleaved exposures is the image.
            (w / 2, Store::words(order, w / 2, w))
        } else {
            (w, Store::words(order, w, w))
        };
        Ok(Stored {
            tiff: None,
            nominal: (width, h),
            width,
            height: h,
            bits: 12,
            packed_flag: false,
            payload_offset: hdr.section_offset,
            declared: payload.len(),
            payload,
            store,
        })
    }

    fn into_decoder(
        self,
        data: &'a [u8],
        meta: RawMetadata,
        info: FrameInfo,
        cfa: &CfaPattern,
        dir: &Directory,
    ) -> Box<dyn RawDecoder + 'a> {
        let colors = color_tile(dir.xtrans.as_ref(), cfa);
        let Stored { width, height, payload_offset, declared, payload, store, .. } = self;
        let (random_access, units, packed) = match &store {
            Store::Compressed(c) => {
                let units = c
                    .block_ranges(payload)
                    .into_iter()
                    .enumerate()
                    .map(|(i, (offset, length))| DecodeUnit {
                        rect: Rect::new(i as u32 * c.header.block_size, 0, c.header.block_width(i as u32), height),
                        offset: (payload_offset + offset) as u64,
                        length: length as u64,
                    })
                    .collect();
                (RandomAccess::Blocks, units, None)
            }
            Store::Tiled { .. } => (RandomAccess::Sequential, Vec::new(), None),
            Store::Rows { bits, order, exposed, .. } => {
                let packed = exposed.map(|row_stride| PackedPayload {
                    data: payload,
                    width,
                    height,
                    bits: *bits,
                    order: *order,
                    row_stride,
                });
                (RandomAccess::Rows, Vec::new(), packed)
            }
        };
        let source = Arc::new(Source { data, payload_offset, declared, width, height, store, colors });
        let (full, part) = (Arc::clone(&source), source);
        let mut dec = SimpleDecoder::new(RawFormat::Raf, meta, info, Box::new(move |ctx| full.decode(ctx)));
        dec.region = Some(Box::new(move |ctx, r| part.decode_region(ctx, r)));
        dec.random_access = random_access;
        dec.units = units;
        dec.packed = packed;
        Box::new(dec)
    }
}
