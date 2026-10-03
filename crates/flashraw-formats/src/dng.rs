//! Adobe DNG (1.0 – 1.7).
//!
//! Supports CFA, LinearRaw and monochrome images stored as strips or tiles with
//! compression 1 (uncompressed, 8–32 bit, integer or float), 7 (lossless JPEG),
//! 8/32946 (deflate, predictors 1/2/3/34892–34895), 34892 (lossy JPEG) and 52546
//! (JPEG XL, `jpegxl` feature). Tiles/strips decode in parallel straight into the
//! output buffer; region decoding touches only intersecting tiles.
//!
//! DNG anchors CFA and black-level patterns at the ActiveArea origin; FlashRaw
//! re-anchors them at the buffer origin (see `flashraw_core::types`).

use crate::common::*;
use flashraw_codecs::{deflate, ljpeg, packed};
use flashraw_core::par::{GridWriter, TileMut};
use flashraw_core::tiff::{Ifd, Tiff, tags};
use flashraw_core::*;

/// Is this TIFF a DNG?
pub fn is_dng(tiff: &Tiff) -> bool {
    tiff.ifds.first().is_some_and(|i| i.has(tags::DNG_VERSION))
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Payload {
    Uncompressed,
    Ljpeg,
    Deflate,
    LossyJpeg,
    JpegXl,
    /// GoPro VC-5: one wavelet bitstream for the whole image.
    Vc5,
}

/// Parsed DNG raw image description shared by full/region decode.
struct DngRaw<'a> {
    data: &'a [u8],
    order: ByteOrder,
    width: u32,
    height: u32,
    cpp: u32,
    bps: u32,
    float: bool,
    payload: Payload,
    predictor: u32,
    tile_w: u32,
    tile_h: u32,
    /// Byte ranges per grid tile (row-major).
    units: Vec<(usize, usize)>,
    linearization: Option<Vec<u16>>,
    dng_bug16: bool,
    vc5_white: u32,
    vc5_phase: Option<flashraw_codecs::vc5::CfaPhase>,
}

/// Open a DNG.
pub fn open<'a>(data: &'a [u8], tiff: Tiff<'a>, opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    let ifd0 = tiff.ifds.first().ok_or_else(|| Error::corrupt("DNG without IFD0"))?;
    let mut meta = RawMetadata::default();
    flashraw_core::exif::read_ifd0(ifd0, &mut meta);
    let version = ifd0.get(tags::DNG_VERSION).map(|e| e.bytes()).unwrap_or(&[1, 0, 0, 0]);
    let mut v4 = [0u8; 4];
    for (i, b) in version.iter().take(4).enumerate() {
        v4[i] = *b;
    }
    meta.dng_version = Some(v4);
    meta.unique_camera_model = ifd0.string(tags::UNIQUE_CAMERA_MODEL);
    if meta.make.is_empty() || meta.model.is_empty() {
        if let Some(u) = &meta.unique_camera_model {
            let mut it = u.splitn(2, ' ');
            if meta.make.is_empty() {
                meta.make = it.next().unwrap_or("").to_string();
            }
            if meta.model.is_empty() {
                meta.model = it.next().unwrap_or("").to_string();
            }
        }
    }
    let cam = db_lookup(&mut meta, "dng");
    let _ = cam;

    let raw_ifd = find_raw_ifd(&tiff).ok_or_else(|| Error::invalid_metadata("DNG has no raw image IFD"))?;
    let (info, raw) = parse_raw(data, &tiff, ifd0, raw_ifd, v4, &meta, opts)?;
    meta.previews = tiff_previews(data, 0, &tiff, info.orientation);
    if let Some(sw) = &meta.software {
        meta.set_extra("software", sw.clone());
    }
    meta.set_extra("dng_version", format!("{}.{}.{}.{}", v4[0], v4[1], v4[2], v4[3]));

    let raw = std::sync::Arc::new(raw);
    let (r1, r2) = (raw.clone(), raw.clone());
    let mut dec = SimpleDecoder::new(RawFormat::Dng, meta, info, Box::new(move |ctx| r1.decode_region(ctx, None)));
    dec.region = Some(Box::new(move |ctx, rect| r2.decode_region(ctx, Some(rect))));
    let tiled = raw.tile_w < raw.width;
    dec.random_access = match raw.payload {
        Payload::Uncompressed if raw.units.len() == 1 => RandomAccess::Rows,
        _ if tiled => RandomAccess::Tiles { width: raw.tile_w, height: raw.tile_h },
        _ if raw.units.len() > 1 => RandomAccess::Strips { rows: raw.tile_h },
        _ => RandomAccess::Sequential,
    };
    dec.units = raw.unit_list();
    if raw.payload == Payload::Uncompressed
        && raw.units.len() == 1
        && !raw.float
        && raw.linearization.is_none()
        && raw.cpp == 1
    {
        let (o, l) = raw.units[0];
        let bits = raw.bps;
        let order =
            if bits == 16 && raw.order == ByteOrder::Little { PackOrder::LsbBytes } else { PackOrder::MsbBytes };
        if let Ok(d) = flashraw_core::bytes::slice_at_lenient(data, o, l) {
            dec.packed = Some(PackedPayload {
                data: d,
                width: raw.width,
                height: raw.height,
                bits,
                order,
                row_stride: (raw.width as usize * bits as usize).div_ceil(8),
            });
        }
    }
    Ok(Box::new(dec))
}

/// The main raw IFD: NewSubFileType 0 with CFA/LinearRaw photometric, largest area.
fn find_raw_ifd<'t, 'a>(tiff: &'t Tiff<'a>) -> Option<&'t Ifd<'a>> {
    tiff.all_ifds()
        .into_iter()
        .filter(|i| i.u32(tags::NEW_SUBFILE_TYPE).unwrap_or(0) == 0)
        .filter(|i| {
            matches!(i.u32(tags::PHOTOMETRIC), Some(32803) | Some(34892))
                || (i.u32(tags::PHOTOMETRIC) == Some(1) && i.bps() > 8)
        })
        .filter(|i| i.width() > 0 && i.height() > 0)
        .fold(None, |best: Option<&'t Ifd<'a>>, i| match best {
            // Keep the first of equally large images: multi-frame DNGs (e.g. Pentax Pixel
            // Shift) store frames in order and frame 0 is the primary image.
            Some(b) if b.width() as u64 * b.height() as u64 >= i.width() as u64 * i.height() as u64 => Some(b),
            _ => Some(i),
        })
}

fn rational_pair(ifd: &Ifd, tag: u16) -> Option<(f64, f64)> {
    let e = ifd.get(tag)?;
    Some((e.f64(0)?, e.f64(1)?))
}

fn parse_raw<'a>(
    data: &'a [u8],
    tiff: &Tiff<'a>,
    ifd0: &Ifd<'a>,
    raw: &Ifd<'a>,
    version: [u8; 4],
    meta: &RawMetadata,
    opts: &OpenOptions,
) -> Result<(FrameInfo, DngRaw<'a>)> {
    let width = raw.width();
    let height = raw.height();
    let cpp = raw.u32(tags::SAMPLES_PER_PIXEL).unwrap_or(1);
    let bps = raw.bps();
    opts.limits.check_dims(width, height, cpp)?;
    if cpp == 0 || cpp > 4 {
        return Err(Error::unsupported_compression("DNG", format!("{cpp} samples per pixel")));
    }
    if raw.u32(tags::PLANAR_CONFIG).unwrap_or(1) != 1 && cpp > 1 {
        return Err(Error::unsupported_compression("DNG", "planar configuration 2"));
    }
    let float = raw.u32(tags::SAMPLE_FORMAT) == Some(3);
    let compression = raw.compression();
    let photometric = raw.u32(tags::PHOTOMETRIC).unwrap_or(32803);
    let (payload, comp_kind) = match compression {
        1 => (Payload::Uncompressed, Compression::Uncompressed),
        7 => (Payload::Ljpeg, Compression::LosslessJpeg),
        8 | 32946 => (Payload::Deflate, Compression::Deflate),
        34892 => (Payload::LossyJpeg, Compression::LossyJpeg),
        52546 => (Payload::JpegXl, Compression::JpegXl),
        9 => (Payload::Vc5, Compression::Vc5),
        c => return Err(Error::unsupported_compression("DNG", format!("TIFF compression {c}"))),
    };
    if payload == Payload::Uncompressed && !float && !(1..=16).contains(&bps) && bps != 32 {
        return Err(Error::unsupported_compression("DNG", format!("{bps}-bit uncompressed")));
    }
    if float && !matches!(bps, 16 | 24 | 32) {
        return Err(Error::unsupported_compression("DNG", format!("{bps}-bit float")));
    }
    // Grid: tiles, or strips as full-width tiles.
    let (tile_w, tile_h, units) = if raw.has(tags::TILE_OFFSETS) {
        let tw = raw.req_u32(tags::TILE_WIDTH)?;
        let th = raw.req_u32(tags::TILE_LENGTH)?;
        (tw, th, tiles(raw))
    } else {
        let rps = raw.u32(tags::ROWS_PER_STRIP).unwrap_or(height).clamp(1, height.max(1));
        (width, rps, strips(raw))
    };
    if tile_w == 0 || tile_h == 0 {
        return Err(Error::invalid_metadata("DNG tile size is zero"));
    }
    let expected = width.div_ceil(tile_w) as usize * height.div_ceil(tile_h) as usize;
    if units.len() < expected {
        // Single-strip files sometimes claim RowsPerStrip < height with one strip; treat as one unit.
        if units.len() != 1 || raw.has(tags::TILE_OFFSETS) {
            return Err(Error::corrupt(format!("DNG has {} tiles/strips, expected {expected}", units.len())));
        }
    }
    let (tile_h, units) = if units.len() == 1 && expected > 1 && !raw.has(tags::TILE_OFFSETS) {
        (height, units)
    } else {
        (tile_h, units)
    };

    let mut info = FrameInfo::new(width, height, bps.min(16) as u8);
    info.cpp = cpp;
    info.sample_format = if float { SampleFormat::F32 } else { SampleFormat::U16 };
    info.compression = comp_kind;
    info.orientation = Orientation::from_exif(ifd0.u32(tags::ORIENTATION).unwrap_or(1));
    let lists = [tags::OPCODE_LIST1, tags::OPCODE_LIST2, tags::OPCODE_LIST3]
        .map(|t| raw.get(t).or_else(|| ifd0.get(t)).map(|e| e.bytes().to_vec()).unwrap_or_default());
    let opcodes = flashraw_core::decoder::OpcodeLists { lists };
    if !opcodes.is_empty() {
        info.opcodes = Some(std::sync::Arc::new(opcodes));
    }

    // Active area & crop.
    let active = raw
        .get(tags::ACTIVE_AREA)
        .and_then(|e| Some((e.u32(0)?, e.u32(1)?, e.u32(2)?, e.u32(3)?)))
        .and_then(|(t, l, b, r)| Rect::from_ltrb(l, t, r, b))
        .filter(|r| !r.is_empty() && r.right() <= width && r.bottom() <= height)
        .unwrap_or(Rect::full(width, height));
    info.active_area = active;
    info.crop = match (rational_pair(raw, tags::DEFAULT_CROP_ORIGIN), rational_pair(raw, tags::DEFAULT_CROP_SIZE)) {
        (Some((ox, oy)), Some((w, h))) if w >= 1.0 && h >= 1.0 && ox >= 0.0 && oy >= 0.0 => {
            Rect::new(active.x + ox.round() as u32, active.y + oy.round() as u32, w.round() as u32, h.round() as u32)
                .intersect(&active)
                .unwrap_or(active)
        }
        (None, Some((w, h))) if w >= 1.0 && h >= 1.0 => {
            Rect::new(active.x, active.y, w as u32, h as u32).intersect(&active).unwrap_or(active)
        }
        _ => active,
    };
    if let Some(m) = raw.get(tags::MASKED_AREAS) {
        let v = m.u32s();
        for c in v.chunks_exact(4) {
            if let Some(r) = Rect::from_ltrb(c[1], c[0], c[3], c[2]) {
                info.masked_areas.push(r);
            }
        }
    }

    // Layout & CFA.
    if photometric == 32803 {
        let layout = raw.u32(tags::CFA_LAYOUT).unwrap_or(1);
        if layout != 1 {
            return Err(Error::unsupported_compression("DNG", format!("CFALayout {layout} (non-rectangular CFA)")));
        }
        let dim = raw
            .get(tags::CFA_REPEAT_PATTERN_DIM)
            .ok_or_else(|| Error::invalid_metadata("DNG CFA without CFARepeatPatternDim"))?;
        let (ph, pw) = (dim.u32(0).unwrap_or(2) as usize, dim.u32(1).unwrap_or(2) as usize);
        let pat = raw.get(tags::CFA_PATTERN_EP).ok_or_else(|| Error::invalid_metadata("DNG CFA without CFAPattern"))?;
        let plane: Vec<u8> =
            raw.get(tags::CFA_PLANE_COLOR).map(|e| e.bytes().to_vec()).unwrap_or_else(|| vec![0, 1, 2]);
        let colors: Option<Vec<CfaColor>> = pat
            .bytes()
            .iter()
            .take(pw * ph)
            .map(|&i| plane.get(i as usize).copied().and_then(CfaColor::from_dng))
            .collect();
        let colors =
            colors.filter(|c| c.len() == pw * ph).ok_or_else(|| Error::invalid_metadata("invalid DNG CFAPattern"))?;
        let p =
            CfaPattern::new(pw, ph, &colors).ok_or_else(|| Error::invalid_metadata("invalid DNG CFA dimensions"))?;
        // Re-anchor from ActiveArea origin to buffer origin.
        let p = p.shifted(pw - active.x as usize % pw, ph - active.y as usize % ph);
        info.layout = layout_for(&p);
        info.cfa = Some(p);
    } else if cpp == 1 {
        info.layout = SensorLayout::Monochrome;
    } else {
        info.layout = SensorLayout::Linear;
    }

    // Linearization.
    let linearization = raw
        .get(tags::LINEARIZATION_TABLE)
        .map(|e| e.u32s().into_iter().map(|v| v.min(65535) as u16).collect::<Vec<_>>())
        .filter(|t| !t.is_empty());

    // Black level (pattern anchored at the active area).
    let (br, bc) = raw
        .get(tags::BLACK_LEVEL_REPEAT_DIM)
        .map(|e| (e.u32(0).unwrap_or(1) as usize, e.u32(1).unwrap_or(1) as usize))
        .unwrap_or((1, 1));
    let (br, bc) = (br.clamp(1, 8), bc.clamp(1, 8));
    if let Some(e) = raw.get(tags::BLACK_LEVEL) {
        let vals = e.f32s();
        let need = br * bc * cpp as usize;
        let values: Vec<f32> = if vals.len() >= need {
            vals[..need].to_vec()
        } else if !vals.is_empty() {
            (0..need).map(|i| vals[i % vals.len()]).collect()
        } else {
            vec![0.0; need]
        };
        let mut b = BlackLevel {
            rows: br as u8,
            cols: bc as u8,
            samples: cpp as u8,
            values,
            row_deltas: vec![],
            col_deltas: vec![],
        };
        b = if active.x as usize % bc != 0 || active.y as usize % br != 0 {
            b.shifted(bc - active.x as usize % bc, br - active.y as usize % br)
        } else {
            b
        };
        if let Some(dh) = raw.get(tags::BLACK_LEVEL_DELTA_H) {
            let d = dh.f32s();
            if d.iter().any(|&v| v != 0.0) {
                let mut cols = vec![0.0f32; width as usize];
                for (i, v) in d.iter().enumerate().take(active.width as usize) {
                    cols[active.x as usize + i] = *v;
                }
                b.col_deltas = cols;
            }
        }
        if let Some(dv) = raw.get(tags::BLACK_LEVEL_DELTA_V) {
            let d = dv.f32s();
            if d.iter().any(|&v| v != 0.0) {
                let mut rows = vec![0.0f32; height as usize];
                for (i, v) in d.iter().enumerate().take(active.height as usize) {
                    rows[active.y as usize + i] = *v;
                }
                b.row_deltas = rows;
            }
        }
        info.black_level = b;
    }
    // White level.
    info.white_level = match raw.get(tags::WHITE_LEVEL) {
        Some(e) if !e.is_empty() => WhiteLevel { values: e.u32s() },
        _ => {
            let max = if float {
                1
            } else if let Some(t) = &linearization {
                t.iter().copied().max().unwrap_or(65535) as u32
            } else if bps >= 16 {
                65535
            } else {
                (1u32 << bps) - 1
            };
            WhiteLevel::uniform(max)
        }
    };

    // Colour.
    let colors =
        if photometric == 32803 { info.cfa.map(|p| p.distinct_colors().len()).unwrap_or(3) } else { cpp as usize };
    let rows = colors.max(3);
    let mut cm = ColorMatrices { source: ColorSource::File, ..Default::default() };
    let ill = |tag: u16| Illuminant(ifd0.u32(tag).unwrap_or(0) as u16);
    for (mtag, itag) in [
        (tags::COLOR_MATRIX1, tags::CALIBRATION_ILLUMINANT1),
        (tags::COLOR_MATRIX2, tags::CALIBRATION_ILLUMINANT2),
        (tags::COLOR_MATRIX3, tags::CALIBRATION_ILLUMINANT3),
    ] {
        if let Some(m) = ifd0.get(mtag).or_else(|| raw.get(mtag)) {
            let v = m.f32s();
            if let Some(x) =
                ColorMatrix::new(ill(itag), v.len() / 3, v.clone()).filter(|x| x.rows as usize == rows || v.len() == 9)
            {
                cm.color.push(x);
            }
        }
    }
    for (mtag, itag) in [
        (tags::FORWARD_MATRIX1, tags::CALIBRATION_ILLUMINANT1),
        (tags::FORWARD_MATRIX2, tags::CALIBRATION_ILLUMINANT2),
        (tags::FORWARD_MATRIX3, tags::CALIBRATION_ILLUMINANT3),
    ] {
        if let Some(m) = ifd0.get(mtag) {
            let v = m.f32s();
            // Forward matrices are 3 x N (XYZ rows); store as-is with rows = 3.
            if v.len() >= 9 {
                cm.forward.push(ColorMatrix { illuminant: ill(itag), rows: 3, values: v });
            }
        }
    }
    for (mtag, itag) in [
        (tags::CAMERA_CALIBRATION1, tags::CALIBRATION_ILLUMINANT1),
        (tags::CAMERA_CALIBRATION2, tags::CALIBRATION_ILLUMINANT2),
        (tags::CAMERA_CALIBRATION3, tags::CALIBRATION_ILLUMINANT3),
    ] {
        if let Some(m) = ifd0.get(mtag) {
            let v = m.f32s();
            let n = (v.len() as f64).sqrt() as usize;
            if n * n == v.len() && n >= 3 {
                cm.calibration.push(ColorMatrix { illuminant: ill(itag), rows: n as u8, values: v });
            }
        }
    }
    if let Some(ab) = ifd0.get(tags::ANALOG_BALANCE) {
        cm.analog_balance = ab.f32s();
    }
    if cm.color.is_empty() {
        cm = db_color(meta, None);
    }
    info.color_matrices = cm;
    if let Some(n) = ifd0.get(tags::AS_SHOT_NEUTRAL) {
        info.white_balance = WhiteBalance::from_neutral(&n.f32s());
    }

    let order = tiff.order;
    let dng_bug16 = version >= [1, 1, 0, 0];
    let tile_w = tile_w.min(width.max(1)).max(if raw.has(tags::TILE_OFFSETS) { tile_w } else { 1 });
    let raw_desc = DngRaw {
        data,
        order,
        width,
        height,
        cpp,
        bps,
        float,
        payload,
        predictor: raw.u32(tags::PREDICTOR).unwrap_or(1),
        tile_w,
        tile_h,
        units,
        linearization,
        dng_bug16,
        vc5_white: info.white_level.max(),
        vc5_phase: info.cfa.as_ref().and_then(flashraw_codecs::vc5::CfaPhase::from_pattern),
    };
    Ok((info, raw_desc))
}

impl DngRaw<'_> {
    fn tiles_x(&self) -> u32 {
        self.width.div_ceil(self.tile_w)
    }

    fn unit_list(&self) -> Vec<DecodeUnit> {
        let tx = self.tiles_x();
        self.units
            .iter()
            .enumerate()
            .map(|(i, &(o, l))| {
                let (x, y) = ((i as u32 % tx) * self.tile_w, (i as u32 / tx) * self.tile_h);
                DecodeUnit {
                    rect: Rect::new(x, y, self.tile_w, self.tile_h).clamp_to(self.width, self.height),
                    offset: o as u64,
                    length: l as u64,
                }
            })
            .collect()
    }

    fn decode_region(&self, ctx: &DecodeContext, region: Option<Rect>) -> Result<RawImage> {
        let full = Rect::full(self.width, self.height);
        let region = match region {
            Some(r) => r.intersect(&full).ok_or_else(|| Error::InvalidArgument(format!("region {r} outside image")))?,
            None => full,
        };
        let mut img = if self.float {
            RawImage::new_f32(region.width, region.height, self.cpp, &ctx.limits)?
        } else {
            RawImage::new_u16(region.width, region.height, self.cpp, &ctx.limits)?
        };
        if self.payload == Payload::Vc5 {
            // One bitstream covers the whole image: decode fully, then crop.
            let mut full_img = RawImage::new_u16(self.width, self.height, 1, &ctx.limits)?;
            let phase =
                self.vc5_phase.ok_or_else(|| Error::unsupported_compression("DNG", "VC-5 without a Bayer CFA"))?;
            let src = self.unit_bytes(0)?;
            ctx.profiler.time(Stage::EntropyDecode, || {
                flashraw_codecs::vc5::decode(
                    src,
                    self.width as usize,
                    self.height as usize,
                    self.vc5_white,
                    phase,
                    full_img.u16_mut(),
                    ctx.parallel,
                )
            })?;
            return if region == full { Ok(full_img) } else { full_img.crop(region) };
        }
        if self.payload == Payload::Uncompressed && self.units.len() == 1 && !self.float && self.bps < 32 {
            ctx.profiler.time(Stage::Unpack, || self.unpack_single_strip(ctx, region, img.u16_mut()))?;
            return Ok(img);
        }
        let stage = match self.payload {
            Payload::Uncompressed => Stage::Unpack,
            _ => Stage::EntropyDecode,
        };
        let (tx, ty) = (self.tiles_x(), self.height.div_ceil(self.tile_h));
        let n = (tx * ty) as usize;
        let errors = std::sync::Mutex::new(Vec::<Error>::new());
        let simd = ctx.simd;
        // Parallelise across units when there are several; a single unit parallelises inside (uncompressed rows).
        let par_units = ctx.parallel && n > 1;
        ctx.profiler.time(stage, || match &mut img.data {
            PixelData::U16(buf) => {
                let g = GridWriter::new(
                    buf,
                    region.width,
                    region.height,
                    self.cpp,
                    (region.x, region.y),
                    self.tile_w,
                    self.tile_h,
                    self.width,
                    self.height,
                )
                .ok_or_else(|| Error::corrupt("invalid DNG tile grid"))?;
                par::for_each_index(n, par_units, |i| {
                    let (x, y) = (i as u32 % tx, i as u32 / tx);
                    if let Some(t) = g.claim(x, y) {
                        if let Err(e) = self.decode_unit_u16(i, t, simd, ctx.parallel && !par_units) {
                            errors.lock().unwrap().push(e);
                        }
                    }
                });
                Ok::<(), Error>(())
            }
            PixelData::F32(buf) => {
                let g = GridWriter::new(
                    buf,
                    region.width,
                    region.height,
                    self.cpp,
                    (region.x, region.y),
                    self.tile_w,
                    self.tile_h,
                    self.width,
                    self.height,
                )
                .ok_or_else(|| Error::corrupt("invalid DNG tile grid"))?;
                par::for_each_index(n, par_units, |i| {
                    let (x, y) = (i as u32 % tx, i as u32 / tx);
                    if let Some(t) = g.claim(x, y) {
                        if let Err(e) = self.decode_unit_f32(i, t) {
                            errors.lock().unwrap().push(e);
                        }
                    }
                });
                Ok(())
            }
        })?;
        if let Some(e) = errors.into_inner().unwrap().into_iter().next() {
            return Err(e);
        }
        Ok(img)
    }

    /// Uncompressed single-strip images: unpack rows in parallel directly into `out`.
    fn unpack_single_strip(&self, ctx: &DecodeContext, region: Rect, out: &mut [u16]) -> Result<()> {
        let src = self.unit_bytes(0)?;
        let cpp = self.cpp as usize;
        let bits = self.bps;
        let order =
            if bits == 16 && self.order == ByteOrder::Little { PackOrder::LsbBytes } else { PackOrder::MsbBytes };
        let full_rl = self.width as usize * cpp;
        let row_bytes = (full_rl * bits as usize).div_ceil(8);
        let out_rl = region.width as usize * cpp;
        let x0 = region.x as usize * cpp;
        let lin = self.linearization.as_deref();
        let simd = ctx.simd;
        par::for_each_row_chunk_bounded(out, out_rl, par::memory_bound_workers(), ctx.parallel, |first, chunk| {
            let mut tmp = if x0 == 0 && out_rl == full_rl { Vec::new() } else { vec![0u16; full_rl] };
            for (i, row) in chunk.chunks_mut(out_rl).enumerate() {
                let y = region.y as usize + first + i;
                let Some(s) = src.get(y * row_bytes..) else { break };
                let s = &s[..row_bytes.min(s.len())];
                if tmp.is_empty() {
                    packed::unpack_row(s, row, bits, order, simd);
                } else {
                    packed::unpack_row(s, &mut tmp, bits, order, simd);
                    row.copy_from_slice(&tmp[x0..x0 + out_rl]);
                }
                if let Some(l) = lin {
                    let last = *l.last().unwrap();
                    for v in row.iter_mut() {
                        *v = l.get(*v as usize).copied().unwrap_or(last);
                    }
                }
            }
        });
        Ok(())
    }

    fn unit_bytes(&self, i: usize) -> Result<&[u8]> {
        let (o, mut l) = *self.units.get(i).ok_or_else(|| Error::corrupt(format!("missing DNG tile {i}")))?;
        if self.payload == Payload::Uncompressed {
            // Some writers (e.g. DJI) understate StripByteCounts; uncompressed data is
            // addressed by geometry, so read what the geometry needs if the file has it.
            let needed = (self.tile_row_len() * self.bps as usize).div_ceil(8) * self.tile_h as usize;
            l = l.max(needed);
        }
        flashraw_core::bytes::slice_at_lenient(self.data, o, l)
    }

    /// Samples per tile row.
    fn tile_row_len(&self) -> usize {
        self.tile_w as usize * self.cpp as usize
    }

    fn decode_unit_u16(&self, i: usize, mut t: TileMut<'_, '_, u16>, simd: bool, parallel_rows: bool) -> Result<()> {
        let src = self.unit_bytes(i)?;
        let (w, h) = t.size();
        let cpp = self.cpp as usize;
        let (sx, sy) = (t.skip_x as usize * cpp, t.skip_y as usize);
        let lin = self.linearization.as_deref();
        let finish = |row: &mut [u16]| {
            if let Some(l) = lin {
                let last = *l.last().unwrap();
                for v in row.iter_mut() {
                    *v = l.get(*v as usize).copied().unwrap_or(last);
                }
            }
        };
        let trl = self.tile_row_len();
        match self.payload {
            Payload::Uncompressed => {
                let bits = self.bps;
                if bits == 32 {
                    // 32-bit integer samples: keep the high 16 bits is lossy; reject explicitly.
                    return Err(Error::unsupported_compression("DNG", "32-bit integer samples"));
                }
                let order = if bits == 16 && self.order == ByteOrder::Little {
                    PackOrder::LsbBytes
                } else {
                    PackOrder::MsbBytes
                };
                let row_bytes = (trl * bits as usize).div_ceil(8);
                let _ = parallel_rows;
                let mut tmp = vec![0u16; trl];
                for y in 0..h {
                    let off = (sy + y) * row_bytes;
                    let s = src.get(off..).unwrap_or(&[]);
                    if s.is_empty() {
                        break; // truncated: remaining rows stay zero
                    }
                    let row = t.row(y);
                    if sx == 0 && w * cpp == trl {
                        packed::unpack_row(&s[..row_bytes.min(s.len())], row, bits, order, simd);
                    } else {
                        packed::unpack_row(&s[..row_bytes.min(s.len())], &mut tmp, bits, order, simd);
                        row.copy_from_slice(&tmp[sx..sx + w * cpp]);
                    }
                    finish(row);
                }
                Ok(())
            }
            Payload::Ljpeg => {
                let hdr = match ljpeg::LjpegHeader::parse(src, ljpeg::LjpegOptions { dng_bug16: self.dng_bug16 }) {
                    Ok(h) => h,
                    Err(Error::UnsupportedCompression { .. }) if cpp == 1 => {
                        return self.decode_dct12_tile(src, &mut t, sx, sy, finish);
                    }
                    Err(e) => return Err(e),
                };
                let (mut row, mut col) = (0usize, 0usize);
                let (cx0, cx1) = (sx, sx + w * cpp);
                ljpeg::decode_rows_par(src, &hdr, parallel_rows, |_, mut s| {
                    while !s.is_empty() && row < sy + h {
                        let take = (trl - col).min(s.len());
                        if row >= sy {
                            let (a, b) = (col.max(cx0), (col + take).min(cx1));
                            if a < b {
                                let r = t.row(row - sy);
                                let dst = &mut r[a - cx0..b - cx0];
                                dst.copy_from_slice(&s[a - col..b - col]);
                                finish(dst);
                            }
                        }
                        s = &s[take..];
                        col += take;
                        if col == trl {
                            col = 0;
                            row += 1;
                        }
                    }
                    Ok(())
                })
            }
            Payload::Deflate => {
                let bytes_ps = self.bps.div_ceil(8) as usize;
                let row_bytes = trl * bytes_ps;
                let raw = deflate::inflate_zlib(src, row_bytes * self.tile_h as usize)?;
                for y in 0..h {
                    let off = (sy + y) * row_bytes;
                    let Some(rb) = raw.get(off..off + row_bytes) else { break };
                    let mut vals: Vec<u16> = match bytes_ps {
                        1 => rb.iter().map(|&b| b as u16).collect(),
                        _ => rb
                            .chunks_exact(2)
                            .map(|c| {
                                if self.order == ByteOrder::Little {
                                    u16::from_le_bytes([c[0], c[1]])
                                } else {
                                    u16::from_be_bytes([c[0], c[1]])
                                }
                            })
                            .collect(),
                    };
                    match self.predictor {
                        2 => deflate::undo_horizontal_u16(&mut vals, cpp, 1),
                        34892 => deflate::undo_horizontal_u16(&mut vals, cpp, 2),
                        34893 => deflate::undo_horizontal_u16(&mut vals, cpp, 4),
                        _ => {}
                    }
                    let r = t.row(y);
                    r.copy_from_slice(&vals[sx..sx + w * cpp]);
                    finish(r);
                }
                Ok(())
            }
            Payload::LossyJpeg => {
                let img = flashraw_codecs::jpeg::decode(src)?;
                let ccount = img.components;
                for y in 0..h {
                    let r = t.row(y);
                    let yy = sy + y;
                    if yy >= img.height {
                        break;
                    }
                    for (xi, v) in r.iter_mut().enumerate() {
                        let x = (sx + xi) / cpp;
                        let c = (sx + xi) % cpp;
                        if x < img.width {
                            *v = img.data[(yy * img.width + x) * ccount + c.min(ccount - 1)] as u16;
                        }
                    }
                    finish(r);
                }
                Ok(())
            }
            Payload::JpegXl => decode_jxl_u16(src, &mut t, sx, sy, cpp, finish),
            Payload::Vc5 => Err(Error::corrupt("VC-5 is decoded as a whole image")),
        }
    }

    /// Blackmagic 12-bit DCT tiles: the JPEG frame is twice the tile width; block
    /// columns in the left half fill even tile rows, the right half odd rows, and each
    /// 8x8 block covers 16 tile rows (every other row).
    fn decode_dct12_tile(
        &self,
        src: &[u8],
        t: &mut TileMut<'_, '_, u16>,
        sx: usize,
        sy: usize,
        finish: impl Fn(&mut [u16]),
    ) -> Result<()> {
        let d = flashraw_codecs::dct12::Dct12::parse(src, self.dng_bug16)?;
        let tw = self.tile_w as usize;
        let (w, h) = t.size();
        d.decode_blocks(src, |jrow, jcol, block| {
            let (jrow, jcol) = (jrow as usize, jcol as usize);
            let row0 = jcol / tw + jrow * 2;
            let col0 = jcol % tw;
            for bi in 0..8 {
                let r = row0 + bi * 2;
                if r < sy || r - sy >= h {
                    continue;
                }
                let line = t.row(r - sy);
                for j in 0..8 {
                    let c = col0 + j;
                    if c >= sx && c - sx < w {
                        line[c - sx] = block[bi * 8 + j];
                    }
                }
            }
        })?;
        for y in 0..h {
            finish(t.row(y));
        }
        Ok(())
    }

    fn decode_unit_f32(&self, i: usize, mut t: TileMut<'_, '_, f32>) -> Result<()> {
        let src = self.unit_bytes(i)?;
        let (w, h) = t.size();
        let cpp = self.cpp as usize;
        let (sx, sy) = (t.skip_x as usize * cpp, t.skip_y as usize);
        let trl = self.tile_row_len();
        let bytes_ps = (self.bps / 8) as usize;
        let row_bytes = trl * bytes_ps;
        thread_local! {
            static SCRATCH: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
        }
        let mut scratch = SCRATCH.with(|c| std::mem::take(&mut *c.borrow_mut()));
        let raw: &[u8] = match self.payload {
            Payload::Uncompressed => src,
            Payload::Deflate => {
                let need = row_bytes * self.tile_h as usize;
                if scratch.len() < need {
                    scratch.resize(need, 0);
                }
                let n = deflate::inflate_zlib_into(src, &mut scratch[..need])?;
                &scratch[..n]
            }
            _ => return Err(Error::unsupported_compression("DNG", "float samples with this compression")),
        };
        let factor = match (self.payload, self.predictor) {
            (Payload::Deflate, 3) => cpp,
            (Payload::Deflate, 34894) => cpp * 2,
            (Payload::Deflate, 34895) => cpp * 4,
            _ => 0,
        };
        let little = self.order == ByteOrder::Little;
        let mut rowbuf = vec![0u8; row_bytes];
        let mut vals = vec![0f32; if sx == 0 && w * cpp == trl { 0 } else { trl }];
        for y in 0..h {
            let off = (sy + y) * row_bytes;
            let Some(rb) = raw.get(off..off + row_bytes) else { break };
            rowbuf.copy_from_slice(rb);
            if vals.is_empty() {
                deflate::decode_float_row(&mut rowbuf, trl, bytes_ps, factor, little, t.row(y));
            } else {
                deflate::decode_float_row(&mut rowbuf, trl, bytes_ps, factor, little, &mut vals);
                t.row(y).copy_from_slice(&vals[sx..sx + w * cpp]);
            }
        }
        // Return the per-thread scratch buffer for the next tile.
        SCRATCH.with(|c| *c.borrow_mut() = scratch);
        Ok(())
    }
}

#[cfg(feature = "jpegxl")]
fn decode_jxl_u16(
    src: &[u8],
    t: &mut TileMut<'_, '_, u16>,
    sx: usize,
    sy: usize,
    cpp: usize,
    finish: impl Fn(&mut [u16]),
) -> Result<()> {
    let img = flashraw_codecs::jxl::decode(src)?;
    let (w, h) = t.size();
    for y in 0..h {
        let yy = sy + y;
        if yy >= img.height {
            break;
        }
        let r = t.row(y);
        for (xi, v) in r.iter_mut().enumerate().take(w * cpp) {
            let x = (sx + xi) / cpp;
            let c = (sx + xi) % cpp;
            if x < img.width {
                *v = img.data[(yy * img.width + x) * img.channels + c.min(img.channels - 1)];
            }
        }
        finish(r);
    }
    Ok(())
}

#[cfg(not(feature = "jpegxl"))]
fn decode_jxl_u16(
    _src: &[u8],
    _t: &mut TileMut<'_, '_, u16>,
    _sx: usize,
    _sy: usize,
    _cpp: usize,
    _finish: impl Fn(&mut [u16]),
) -> Result<()> {
    Err(Error::unsupported_compression("DNG", "JPEG XL (build with the `jpegxl` feature)"))
}
