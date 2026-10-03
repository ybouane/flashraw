//! Sony ARW / SR2 / SRF / ARQ.
//!
//! All Sony raws are TIFF containers. The sensor image lives in a SubIFD (or IFD0
//! for SRF) and is stored as one of:
//!
//! | Compression | Payload | Decoder |
//! |---|---|---|
//! | 32767, `bytes == w*h` | ARW2 lossy 8-bit-per-pixel delta blocks + tone curve | [`flashraw_codecs::sony_arw2`] |
//! | 32767, `bytes*8 != w*h*bps` | ARW1 Huffman column stream (A100–A390) | [`flashraw_codecs::sony_arw1`] |
//! | 32767, `bytes*8 == w*h*12` | 12-bit LSB-first packed (A850/A900) | [`flashraw_codecs::packed`] |
//! | 1 or 32767, `bytes == w*h*2` | 16-bit containers (12/14-bit data) | [`flashraw_codecs::packed`] |
//! | 1, 4 samples | ARQ pixel-shift composite (R, G, B, G2 per pixel) | [`flashraw_codecs::packed`] |
//! | 7 / 6 (Bayer) | lossless JPEG tiles, each JPEG MCU = one 2x2 quad | [`flashraw_codecs::ljpeg`] |
//! | 6, YCbCr | "lossless compressed" S/M sizes: 4:2:0/4:2:2 lossless JPEG tiles | [`flashraw_codecs::sony_ycc`] |
//! | SRF (DSC-F828/V3) | encrypted big-endian 16-bit samples | [`crypt`] |
//!
//! Black levels and white balance come from the raw IFD (`0x7310`/`0x7313`/`0xC61D`
//! on recent bodies) or from the encrypted SR2Private sub-IFD reached through
//! `DNGPrivateData` (0xC634). Geometry follows per-model visible-area rules.

pub mod crypt;
#[cfg(test)]
mod robustness;
mod srf;
pub(crate) mod table;

use crate::common::*;
use crate::minolta;
use flashraw_codecs::{ljpeg, packed, sony_arw1, sony_arw2, sony_ycc};
use flashraw_core::par::GridWriter;
use flashraw_core::tiff::{Ifd, Tiff, tags};
use flashraw_core::*;

/// Sony maker tags inside the raw IFD / SR2 sub-IFD.
mod stag {
    pub const CURVE: u16 = 0x7010;
    pub const SR2_PRIVATE_OFFSET: u16 = 0x7200;
    pub const SR2_PRIVATE_LENGTH: u16 = 0x7201;
    pub const SR2_PRIVATE_KEY: u16 = 0x7221;
    pub const BLACK_RGBG: u16 = 0x7300;
    pub const WB_GRBG: u16 = 0x7303;
    pub const BLACK_RGGB: u16 = 0x7310;
    pub const WB_RGGB: u16 = 0x7313;
    pub const CROP_ORIGIN: u16 = 0x74C7;
    pub const CROP_SIZE: u16 = 0x74C8;
}

/// Magic-signature probe (Sony raws are dispatched by TIFF make).
pub fn probe(_data: &[u8]) -> bool {
    false
}

pub fn open<'a>(data: &'a [u8], opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    let tiff = Tiff::parse(data)?;
    open_tiff(data, tiff, opts)
}

/// Payload layout of the sensor image.
#[derive(Debug, Clone)]
enum Payload {
    Arw2 {
        curve: Vec<u16>,
    },
    Arw1 {
        coded_rows: usize,
        stored_rows: usize,
    },
    /// 16-bit containers; `big_endian` for the DSC-R1.
    Unpacked16 {
        big_endian: bool,
    },
    /// 12-bit packed, LSB-first (A850/A900) or MSB-first.
    Packed12 {
        lsb: bool,
    },
    /// ARQ: 4 samples per pixel, stored R, G, G2, B -> output R, G, B, G2.
    Arq,
    /// Lossless JPEG tiles; each JPEG MCU (4 components) is a 2x2 Bayer quad.
    LjpegTiles {
        tiles: Vec<(usize, usize)>,
        tile_w: u32,
        tile_h: u32,
    },
    /// ARW format 6 wavelet strip (ILCE-7M5 "compressed").
    Arw6,
    /// YCbCr lossless JPEG tiles converted to RGB.
    Ycbcr {
        tiles: Vec<(usize, usize)>,
        tile_w: u32,
        tile_h: u32,
    },
}

/// Levels/WB gathered from the raw IFD and the SR2 private directory, in
/// (R, G, B, G2) channel order.
#[derive(Debug, Default, Clone)]
struct Levels {
    black: Option<u32>,
    cblack: [u32; 4],
    cam_mul: Option<[f32; 4]>,
    white: Option<u32>,
}

impl Levels {
    /// Apply one Sony level/WB tag (values already read as u16).
    fn apply(&mut self, tag: u16, v: &[u32]) {
        if v.len() < 4 {
            return;
        }
        match tag {
            stag::BLACK_RGBG => self.cblack.copy_from_slice(&v[..4]),
            stag::BLACK_RGGB => {
                // RGGB -> RGBG
                let c = [v[0], v[1], v[3], v[2]];
                let m = *c.iter().min().unwrap_or(&0);
                self.cblack = c.map(|x| x - m);
                self.black = Some(m);
            }
            stag::WB_GRBG => self.cam_mul = Some([v[1] as f32, v[0] as f32, v[2] as f32, v[3] as f32]),
            stag::WB_RGGB => self.cam_mul = Some([v[0] as f32, v[1] as f32, v[3] as f32, v[2] as f32]),
            _ => {}
        }
    }
}

pub fn open_tiff<'a>(data: &'a [u8], tiff: Tiff<'a>, opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    let mut meta = RawMetadata::default();
    for ifd in &tiff.ifds {
        flashraw_core::exif::read_ifd0(ifd, &mut meta);
    }
    let model = meta.model.clone();
    if srf::is_srf(&model) {
        return srf::open(data, &tiff, meta, opts);
    }
    if model == "DSLR-A100" {
        return open_a100(data, &tiff, meta, opts);
    }

    let raw = find_raw_ifd(&tiff).ok_or_else(|| Error::invalid_metadata("Sony: no raw image IFD"))?;
    let (raw_w, raw_h) = (raw.width(), raw.height());
    let bps = raw.bps();
    let samples = raw.u32(tags::SAMPLES_PER_PIXEL).unwrap_or(1);
    let comp = raw.compression();
    let photometric = raw.u32(tags::PHOTOMETRIC).unwrap_or(32803);
    let bytes: u64 = strips(raw).iter().map(|&(_, l)| l as u64).sum();
    let area = raw_w as u64 * raw_h as u64;
    // The DSC-R1 writes big-endian samples into a little-endian container.
    let big_endian = raw_w == 3984;

    // Select the payload decoder by compression and size.
    let (payload, tiff_bps, compression, mode) = match comp {
        32767 if bytes == area => {
            let knots: Vec<u16> =
                raw.get(stag::CURVE).map(|e| e.u32s().iter().map(|&v| v as u16).collect()).unwrap_or_default();
            (Payload::Arw2 { curve: sony_arw2::build_curve(&knots) }, 14, Compression::SonyArw2, "arw2")
        }
        32767 | 1 if bytes == area * 2 => {
            (Payload::Unpacked16 { big_endian }, 14, Compression::Uncompressed, "uncompressed")
        }
        1 if samples == 4 && bytes == area * 8 => (Payload::Arq, 14, Compression::Uncompressed, "arq"),
        32767 if bytes * 8 != area * bps as u64 => {
            (Payload::Arw1 { coded_rows: raw_h as usize + 8, stored_rows: 0 }, 12, Compression::SonyArw1, "arw1")
        }
        32767 | 1 if bps == 12 => {
            (Payload::Packed12 { lsb: comp == 32767 }, 12, Compression::Uncompressed, "12bit-packed")
        }
        1 if bps == 14 || bps == 16 => {
            (Payload::Unpacked16 { big_endian }, bps, Compression::Uncompressed, "uncompressed")
        }
        32766 if photometric == 32803 && samples == 1 && (bps == 12 || bps == 14) => {
            (Payload::Arw6, 14, Compression::Other, "arw6")
        }
        6 | 7 if photometric == 6 && samples == 3 => {
            let (tiles, tile_w, tile_h) = tile_grid(raw)?;
            (Payload::Ycbcr { tiles, tile_w, tile_h }, 14, Compression::SonyLossless, "lossless-ycbcr")
        }
        6 | 7 if samples == 1 => {
            let (tiles, tile_w, tile_h) = tile_grid(raw)?;
            (Payload::LjpegTiles { tiles, tile_w, tile_h }, bps.clamp(8, 16), Compression::SonyLossless, "lossless")
        }
        c => {
            return Err(Error::unsupported_compression(
                "ARW",
                format!("compression {c}, {bps} bps, {samples} samples"),
            ));
        }
    };
    // ARW1 streams carry 8 rows more than the IFD advertises.
    let buf_h = if matches!(payload, Payload::Arw1 { .. }) { raw_h + 8 } else { raw_h };
    let cpp = if matches!(payload, Payload::Arq | Payload::Ycbcr { .. }) { 4 } else { 1 };
    opts.limits.check_dims(raw_w, buf_h, cpp)?;

    // Levels: raw IFD tags first, then the SR2 private directory (parsed last, so
    // it wins).
    let mut lv = Levels::default();
    for tag in [stag::WB_GRBG, stag::BLACK_RGGB, stag::WB_RGGB] {
        if let Some(e) = raw.get(tag) {
            lv.apply(tag, &e.u32s());
        }
    }
    if let Some(e) = raw.get(tags::WHITE_LEVEL) {
        lv.white = e.u32(0);
    }
    if let Some(sr2) = read_sr2_private(data, &tiff) {
        for (tag, vals) in sr2 {
            lv.apply(tag, &vals);
        }
    }

    let is_arw2 = matches!(payload, Payload::Arw2 { .. });
    let is_packed = matches!(payload, Payload::Packed12 { .. });
    // Defaults when the file carries no black level.
    if lv.black.unwrap_or(0) == 0 && lv.cblack[0] == 0 && raw_w > 3888 {
        lv.black = Some(if is_arw2 { 512 } else { 128 << tiff_bps.saturating_sub(12).min(4) });
    }
    if raw_w == 4600 {
        // A290/A350/A380 have no black offset.
        lv.black = Some(0);
    }

    db_lookup(&mut meta, mode);
    let entry = table::lookup("Sony", table_model(&model));
    let mut black = lv.black.unwrap_or(0);
    table::apply_levels(entry, &mut black, &mut lv.cblack, &mut lv.white);
    let mut white = lv.white.unwrap_or((1u32 << tiff_bps.min(16)) - 1);
    if is_packed {
        // 12-bit bodies whose metadata is in 14-bit units.
        white = white.min(4095);
        if black > 256 || lv.cblack[0] > 256 {
            black /= 4;
            lv.cblack = lv.cblack.map(|c| c / 4);
        }
    }

    if matches!(payload, Payload::Arw6) {
        // The coder works on its own code scale: fixed levels of its output curve.
        black = 1024;
        lv.cblack = [0; 4];
        white = 39002;
    }
    let vis = visible_area(&model, raw_w, buf_h, &payload);
    let mut payload = payload;
    if let Payload::Arw1 { stored_rows, .. } = &mut payload {
        // Rows below the visible height are decoded but not stored.
        *stored_rows = vis.bottom() as usize;
    }
    let mut info = FrameInfo::new(raw_w, buf_h, tiff_bps.min(16) as u8);
    info.compression = compression;
    info.active_area = vis;
    info.crop = default_crop(raw).and_then(|c| c.intersect(&vis)).unwrap_or(vis);
    info.orientation = file_orientation(&tiff);
    if cpp == 4 {
        // ARQ carries four real planes (R, G, B, G2); lossless S/M YCbCr becomes 3-channel RGB.
        info.cpp = if matches!(payload, Payload::Ycbcr { .. }) { 3 } else { 4 };
        info.cfa = None;
        info.layout = SensorLayout::Linear;
        info.black_level = BlackLevel::uniform(black as f32);
    } else {
        let cfa = tiff_ep_cfa(raw).filter(|c| c.is_bayer()).unwrap_or(CfaPattern::RGGB);
        info.layout = layout_for(&cfa);
        info.black_level = cfa_black(&cfa, black, &lv.cblack);
        info.cfa = Some(cfa);
    }
    info.white_level = WhiteLevel::uniform(white);
    if let Payload::Ycbcr { .. } = payload {
        // Converted to RGB with the chroma offset removed; fixed estimated levels
        // for this mode.
        info.black_level = BlackLevel::uniform(1024.0);
        info.white_level = WhiteLevel::uniform(17536);
    }
    info.white_balance = lv.cam_mul.and_then(|m| sane_wb(&m));
    info.color_matrices = table::color(entry, &meta);
    meta.previews = tiff_previews(data, 0, &tiff, info.orientation);
    // Tiled raw IFDs may also carry JPEG pointers into the sensor data: drop any
    // "preview" that starts inside the raw payload.
    let raw_ranges: Vec<(usize, usize)> = match &payload {
        Payload::LjpegTiles { tiles, .. } | Payload::Ycbcr { tiles, .. } => tiles.clone(),
        _ => strips(raw),
    };
    meta.previews.retain(|p| {
        !raw_ranges.iter().any(|&(o, l)| (p.offset as usize) >= o && (p.offset as usize) < o.saturating_add(l))
    });

    let src = payload_bytes(data, raw, &payload)?;
    build_decoder(data, meta, info, payload, src, raw_w, buf_h)
}

/// Whole-payload slice for strip-based payloads (empty for tiles).
fn payload_bytes<'a>(data: &'a [u8], raw: &Ifd, payload: &Payload) -> Result<&'a [u8]> {
    match payload {
        Payload::LjpegTiles { .. } | Payload::Ycbcr { .. } => Ok(&[]),
        _ => contiguous_payload(data, raw),
    }
}

/// Assemble the decoder object for a parsed payload.
fn build_decoder<'a>(
    data: &'a [u8],
    meta: RawMetadata,
    info: FrameInfo,
    payload: Payload,
    src: &'a [u8],
    width: u32,
    height: u32,
) -> Result<Box<dyn RawDecoder + 'a>> {
    let p = std::sync::Arc::new(payload);
    let (p1, p2) = (p.clone(), p.clone());
    let row_based =
        matches!(*p, Payload::Arw2 { .. } | Payload::Unpacked16 { .. } | Payload::Packed12 { .. } | Payload::Arq);
    let tiled = matches!(*p, Payload::LjpegTiles { .. } | Payload::Ycbcr { .. });
    let mut dec = SimpleDecoder::new(
        RawFormat::Arw,
        meta,
        info,
        Box::new(move |ctx| decode_region(data, src, &p1, width, height, ctx, None)),
    );
    if row_based || tiled {
        dec.region = Some(Box::new(move |ctx, r| decode_region(data, src, &p2, width, height, ctx, Some(r))));
        if row_based {
            dec.random_access = RandomAccess::Rows;
        }
    }
    match &*p {
        Payload::LjpegTiles { tiles, tile_w, tile_h } | Payload::Ycbcr { tiles, tile_w, tile_h } => {
            dec.random_access = RandomAccess::Tiles { width: *tile_w, height: *tile_h };
            let tx = width.div_ceil(*tile_w).max(1);
            dec.units = tiles
                .iter()
                .enumerate()
                .map(|(i, &(o, l))| DecodeUnit {
                    rect: Rect::new((i as u32 % tx) * tile_w, (i as u32 / tx) * tile_h, *tile_w, *tile_h)
                        .clamp_to(width, height),
                    offset: o as u64,
                    length: l as u64,
                })
                .collect();
        }
        Payload::Unpacked16 { big_endian } => {
            dec.packed = Some(PackedPayload {
                data: src,
                width,
                height,
                bits: 16,
                order: if *big_endian { PackOrder::MsbBytes } else { PackOrder::LsbBytes },
                row_stride: width as usize * 2,
            });
        }
        Payload::Packed12 { lsb } => {
            dec.packed = Some(PackedPayload {
                data: src,
                width,
                height,
                bits: 12,
                order: if *lsb { PackOrder::LsbBytes } else { PackOrder::MsbBytes },
                row_stride: width as usize * 3 / 2,
            });
        }
        _ => {}
    }
    Ok(Box::new(dec))
}

/// Decode the full buffer or, for row-addressable payloads, only `region`.
fn decode_region(
    data: &[u8],
    src: &[u8],
    payload: &Payload,
    width: u32,
    height: u32,
    ctx: &DecodeContext,
    region: Option<Rect>,
) -> Result<RawImage> {
    let full = Rect::full(width, height);
    let region = match region {
        Some(r) => r.intersect(&full).ok_or_else(|| Error::InvalidArgument(format!("region {r} outside image")))?,
        None => full,
    };
    let w = width as usize;
    match payload {
        Payload::Arw2 { curve } => {
            let mut img = RawImage::new_u16(region.width, region.height, 1, &ctx.limits)?;
            check_rows(src, w, height as usize, "ARW2")?;
            let (x0, rw) = (region.x as usize, region.width as usize);
            ctx.profiler.time(Stage::EntropyDecode, || {
                par::for_each_row_chunk(img.u16_mut(), rw, 16, ctx.parallel, |first, chunk| {
                    let mut tmp = if rw == w { Vec::new() } else { vec![0u16; w] };
                    for (i, row) in chunk.chunks_mut(rw).enumerate() {
                        let y = region.y as usize + first + i;
                        let s = src.get(y * w..).unwrap_or(&[]);
                        let s = &s[..w.min(s.len())];
                        if tmp.is_empty() {
                            sony_arw2::decode_row(s, row, curve);
                        } else {
                            sony_arw2::decode_row(s, &mut tmp, curve);
                            row.copy_from_slice(&tmp[x0..x0 + rw]);
                        }
                    }
                });
            });
            Ok(img)
        }
        Payload::Unpacked16 { big_endian } => {
            let order = if *big_endian { PackOrder::MsbBytes } else { PackOrder::LsbBytes };
            unpack_rows(src, w, height as usize, 16, w * 2, order, region, 1, ctx)
        }
        Payload::Packed12 { lsb } => {
            let order = if *lsb { PackOrder::LsbBytes } else { PackOrder::MsbBytes };
            unpack_rows(src, w, height as usize, 12, w * 3 / 2, order, region, 1, ctx)
        }
        Payload::Arq => {
            // Stored R, G, G2, B per pixel; the output order is R, G, B, G2.
            check_rows(src, w * 8, height as usize, "ARQ")?;
            let mut img = RawImage::new_u16(region.width, region.height, 4, &ctx.limits)?;
            let rl = region.width as usize * 4;
            ctx.profiler.time(Stage::Unpack, || {
                par::for_each_row_chunk_bounded(
                    img.u16_mut(),
                    rl,
                    par::memory_bound_workers(),
                    ctx.parallel,
                    |first, chunk| {
                        for (i, row) in chunk.chunks_mut(rl).enumerate() {
                            let y = region.y as usize + first + i;
                            let s = &src[y * w * 8 + region.x as usize * 8..];
                            for (o, b) in row.chunks_exact_mut(4).zip(s.chunks_exact(8)) {
                                o[0] = u16::from_le_bytes([b[0], b[1]]);
                                o[1] = u16::from_le_bytes([b[2], b[3]]);
                                o[2] = u16::from_le_bytes([b[6], b[7]]);
                                o[3] = u16::from_le_bytes([b[4], b[5]]);
                            }
                        }
                    },
                );
            });
            Ok(img)
        }
        Payload::Arw1 { coded_rows, stored_rows } => {
            let mut img = RawImage::new_u16(width, height, 1, &ctx.limits)?;
            ctx.profiler.time(Stage::EntropyDecode, || {
                sony_arw1::decode_par(src, img.u16_mut(), w, *coded_rows, *stored_rows, ctx.parallel)
            })?;
            if region == full { Ok(img) } else { img.crop(region) }
        }
        Payload::LjpegTiles { tiles, tile_w, tile_h } => {
            let mut img = RawImage::new_u16(region.width, region.height, 1, &ctx.limits)?;
            ctx.profiler.time(Stage::EntropyDecode, || {
                decode_ljpeg_tiles(data, tiles, *tile_w, *tile_h, width, height, region, img.u16_mut(), ctx)
            })?;
            Ok(img)
        }
        Payload::Arw6 => {
            let mut img = RawImage::new_u16(width, height, 1, &ctx.limits)?;
            ctx.profiler.time(Stage::EntropyDecode, || {
                flashraw_codecs::sony_arw6::decode(src, img.u16_mut(), w, height as usize, ctx.parallel)
            })?;
            if region == full { Ok(img) } else { img.crop(region) }
        }
        Payload::Ycbcr { tiles, tile_w, tile_h } => {
            let mut img = RawImage::new_u16(region.width, region.height, 4, &ctx.limits)?;
            ctx.profiler.time(Stage::EntropyDecode, || {
                decode_ycc_tiles(data, tiles, *tile_w, *tile_h, width, height, region, img.u16_mut(), ctx)
            })?;
            // The tile decoder writes R, G, B, 0 (a 4-component layout); keep the 3 real channels.
            let mut rgb = RawImage::new_u16(region.width, region.height, 3, &ctx.limits)?;
            for (o, i) in rgb.u16_mut().chunks_exact_mut(3).zip(img.u16().chunks_exact(4)) {
                o.copy_from_slice(&i[..3]);
            }
            Ok(rgb)
        }
    }
}

/// Require at least one full row of payload for every buffer row (truncated files
/// are corrupt).
fn check_rows(src: &[u8], row_bytes: usize, rows: usize, what: &str) -> Result<()> {
    if src.len() < row_bytes * rows {
        return Err(Error::corrupt(format!("{what}: payload truncated ({} of {} bytes)", src.len(), row_bytes * rows)));
    }
    Ok(())
}

/// Unpack fixed-size rows of `samples` values (row-parallel), restricted to `region`.
#[allow(clippy::too_many_arguments)]
fn unpack_rows(
    src: &[u8],
    samples: usize,
    rows: usize,
    bits: u32,
    stride: usize,
    order: PackOrder,
    region: Rect,
    cpp: usize,
    ctx: &DecodeContext,
) -> Result<RawImage> {
    check_rows(src, stride, rows, "Sony uncompressed")?;
    let mut img = RawImage::new_u16(region.width, region.height, cpp as u32, &ctx.limits)?;
    let rl = region.width as usize * cpp;
    let x0 = region.x as usize * cpp;
    let simd = ctx.simd;
    ctx.profiler.time(Stage::Unpack, || {
        par::for_each_row_chunk_bounded(
            img.u16_mut(),
            rl,
            par::memory_bound_workers(),
            ctx.parallel,
            |first, chunk| {
                let mut tmp = if rl == samples { Vec::new() } else { vec![0u16; samples] };
                for (i, row) in chunk.chunks_mut(rl).enumerate() {
                    let y = region.y as usize + first + i;
                    let s = &src[y * stride..y * stride + stride];
                    if tmp.is_empty() {
                        packed::unpack_row(s, row, bits, order, simd);
                    } else {
                        packed::unpack_row(s, &mut tmp, bits, order, simd);
                        row.copy_from_slice(&tmp[x0..x0 + rl]);
                    }
                }
            },
        );
    });
    Ok(img)
}

/// Lossless JPEG tiles: each MCU of a 4-component JPEG row is a 2x2 Bayer quad.
/// Only tiles intersecting `region` are decoded; `out` covers `region`.
#[allow(clippy::too_many_arguments)]
fn decode_ljpeg_tiles(
    data: &[u8],
    tiles: &[(usize, usize)],
    tile_w: u32,
    tile_h: u32,
    width: u32,
    height: u32,
    region: Rect,
    out: &mut [u16],
    ctx: &DecodeContext,
) -> Result<()> {
    let g = GridWriter::new(out, region.width, region.height, 1, (region.x, region.y), tile_w, tile_h, width, height)
        .ok_or_else(|| Error::corrupt("Sony: invalid tile grid"))?;
    let tx = g.tiles_x();
    let n = (tx * g.tiles_y()) as usize;
    if tiles.len() < n {
        return Err(Error::corrupt(format!("Sony: {} tiles present, {n} expected", tiles.len())));
    }
    let errors = std::sync::Mutex::new(Vec::<Error>::new());
    par::for_each_index(n, ctx.parallel, |i| {
        let Some(mut t) = g.claim(i as u32 % tx, i as u32 / tx) else { return };
        let res = (|| -> Result<()> {
            let (o, l) = tiles[i];
            let src = flashraw_core::bytes::slice_at_lenient(data, o, l)?;
            let hdr = ljpeg::LjpegHeader::parse(src, ljpeg::LjpegOptions::default())?;
            if hdr.clrs != 4 {
                return Err(Error::unsupported_compression(
                    "ARW",
                    format!("lossless tile with {} components", hdr.clrs),
                ));
            }
            let (tw, th) = t.size();
            let (sx, sy) = (t.skip_x as usize, t.skip_y as usize);
            // Quads are written only while they fit the image (the bound for odd
            // sizes): limits in tile-local coordinates.
            let ty0 = (i as u32 / tx * tile_h) as usize;
            let tx0 = (i as u32 % tx * tile_w) as usize;
            let max_rows = (height as usize - 1).saturating_sub(ty0).div_ceil(2) * 2;
            let max_cols = (width as usize - 1).saturating_sub(tx0).div_ceil(2) * 2;
            let (c0, c1) = (sx, (sx + tw).min(max_cols));
            ljpeg::decode_rows(src, &hdr, |jrow, s| {
                let r = jrow * 2;
                if r >= max_rows {
                    return Ok(());
                }
                let c1 = c1.min(s.len() / 4 * 2);
                for k in 0..2 {
                    let rr = r + k;
                    if rr < sy || rr >= sy + th || c0 >= c1 {
                        continue;
                    }
                    let row = t.row(rr - sy);
                    for c in c0..c1 {
                        row[c - sx] = s[(c >> 1) * 4 + 2 * k + (c & 1)];
                    }
                }
                Ok(())
            })
        })();
        if let Err(e) = res {
            errors.lock().unwrap_or_else(|p| p.into_inner()).push(e);
        }
    });
    match errors.into_inner().unwrap_or_else(|p| p.into_inner()).into_iter().next() {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// YCbCr lossless tiles, converted to RGB (4 samples per pixel, 4th = 0). Only
/// tiles intersecting `region` are decoded; `out` covers `region`.
#[allow(clippy::too_many_arguments)]
fn decode_ycc_tiles(
    data: &[u8],
    tiles: &[(usize, usize)],
    tile_w: u32,
    tile_h: u32,
    width: u32,
    height: u32,
    region: Rect,
    out: &mut [u16],
    ctx: &DecodeContext,
) -> Result<()> {
    let g = GridWriter::new(out, region.width, region.height, 4, (region.x, region.y), tile_w, tile_h, width, height)
        .ok_or_else(|| Error::corrupt("Sony: invalid tile grid"))?;
    let tx = g.tiles_x();
    let n = (tx * g.tiles_y()) as usize;
    if tiles.len() < n {
        return Err(Error::corrupt(format!("Sony: {} tiles present, {n} expected", tiles.len())));
    }
    let errors = std::sync::Mutex::new(Vec::<Error>::new());
    par::for_each_index(n, ctx.parallel, |i| {
        let Some(mut t) = g.claim(i as u32 % tx, i as u32 / tx) else { return };
        let res = (|| -> Result<()> {
            let (o, l) = tiles[i];
            let src = flashraw_core::bytes::slice_at_lenient(data, o, l)?;
            let ycc = sony_ycc::decode_tile(src, tile_w as usize, tile_h as usize)?;
            let (tw, th) = t.size();
            let (sx, sy) = (t.skip_x as usize, t.skip_y as usize);
            for y in 0..th {
                let row = t.row(y);
                let s = &ycc[((sy + y) * tile_w as usize + sx) * 3..];
                for (x, px) in row.chunks_exact_mut(4).take(tw).enumerate() {
                    let rgb = sony_ycc::to_rgb(s[3 * x], s[3 * x + 1], s[3 * x + 2]);
                    px[..3].copy_from_slice(&rgb);
                    px[3] = 0;
                }
            }
            Ok(())
        })();
        if let Err(e) = res {
            errors.lock().unwrap_or_else(|p| p.into_inner()).push(e);
        }
    });
    match errors.into_inner().unwrap_or_else(|p| p.into_inner()).into_iter().next() {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// Tile byte ranges plus the tile width and height.
type TileGrid = (Vec<(usize, usize)>, u32, u32);

fn tile_grid(raw: &Ifd) -> Result<TileGrid> {
    let tw = raw.req_u32(tags::TILE_WIDTH)?;
    let th = raw.req_u32(tags::TILE_LENGTH)?;
    if tw == 0 || th == 0 || tw > raw.width().max(1) * 2 || th > raw.height().max(1) * 2 {
        return Err(Error::invalid_metadata(format!("Sony: invalid tile size {tw}x{th}")));
    }
    let t = tiles(raw);
    if t.is_empty() {
        return Err(Error::invalid_metadata("Sony: tiled image without tile offsets"));
    }
    Ok((t, tw, th))
}

/// The raw IFD: largest `width * height * bps` image that is not a preview.
fn find_raw_ifd<'t, 'a>(tiff: &'t Tiff<'a>) -> Option<&'t Ifd<'a>> {
    let mut best: Option<(&Ifd<'a>, u64)> = None;
    for ifd in tiff.all_ifds() {
        let (w, h) = (ifd.width(), ifd.height());
        if w == 0 || h == 0 || w > 65535 || h > 65535 {
            continue;
        }
        let comp = ifd.compression();
        let samples = ifd.u32(tags::SAMPLES_PER_PIXEL).unwrap_or(1);
        let bps = ifd.bps();
        let phot = ifd.u32(tags::PHOTOMETRIC).unwrap_or(0);
        // JPEG previews (unless 12+ bit YCbCr raws) and 8-bit RGB lossless previews.
        if comp == 6 && samples == 3 && bps < 12 {
            continue;
        }
        if bps == 8 && phot == 6 && comp == 7 && samples == 3 && ifd.u32(tags::NEW_SUBFILE_TYPE) == Some(1) {
            continue;
        }
        if bps > 32 || samples > 12 || !(ifd.has(tags::STRIP_OFFSETS) || ifd.has(tags::TILE_OFFSETS)) {
            continue;
        }
        let score = w as u64 * h as u64 * bps.max(1) as u64;
        if best.is_none_or(|(_, s)| score > s) {
            best = Some((ifd, score));
        }
    }
    best.map(|(i, _)| i)
}

/// DefaultCrop (Sony stores it in the raw IFD as DNG tags or 0x74C7/0x74C8).
fn default_crop(raw: &Ifd) -> Option<Rect> {
    let pair = |t: u16| raw.get(t).and_then(|e| Some((e.u32(0)?, e.u32(1)?)));
    let (x, y) = pair(stag::CROP_ORIGIN).or_else(|| pair(tags::DEFAULT_CROP_ORIGIN))?;
    let (w, h) = pair(stag::CROP_SIZE).or_else(|| pair(tags::DEFAULT_CROP_SIZE))?;
    (w > 0 && h > 0).then(|| Rect::new(x, y, w, h))
}

/// First non-default orientation in IFD order (IFD0, its sub-IFDs, then IFD1...).
fn file_orientation(tiff: &Tiff) -> Orientation {
    tiff.all_ifds()
        .into_iter()
        .filter_map(|i| i.u32(tags::ORIENTATION))
        .find(|&o| (2..=8).contains(&o))
        .map(Orientation::from_exif)
        .unwrap_or(Orientation::Normal)
}

/// White balance in (R, G, B, G2) order, rejected when any
/// multiplier is zero or the spread is implausible.
fn sane_wb(m: &[f32; 4]) -> Option<WhiteBalance> {
    if m[..3].iter().any(|&v| v <= 0.001) {
        return None;
    }
    let min = m[..3].iter().copied().fold(f32::MAX, f32::min);
    let max = m[..3].iter().copied().fold(0.0, f32::max);
    if max / min > 100.0 {
        return None;
    }
    WhiteBalance::from_multipliers(m[0], m[1], m[2], m[3])
}

/// Black pattern anchored at (0, 0) from a total black and per-colour deltas in
/// (R, G, B, G2) order; the first green in row-major order gets G, the other G2.
pub(crate) fn cfa_black(cfa: &CfaPattern, black: u32, cblack: &[u32; 4]) -> BlackLevel {
    let mut v = [0f32; 4];
    let mut seen_g = false;
    for y in 0..2 {
        for x in 0..2 {
            let idx = match cfa.color_at(x, y) {
                CfaColor::Red => 0,
                CfaColor::Green if !seen_g => {
                    seen_g = true;
                    1
                }
                CfaColor::Green => 3,
                CfaColor::Blue => 2,
                _ => 1,
            };
            v[y * 2 + x] = (black + cblack[idx]) as f32;
        }
    }
    if v.iter().all(|&b| b == v[0]) { BlackLevel::uniform(v[0]) } else { BlackLevel::pattern2x2(v) }
}

/// Model name used for the colour table: bodies sold under another name share the
/// table entry of the camera they are built on.
fn table_model(model: &str) -> &str {
    match model {
        "UMC-R10C" => "ILCE-QX1",
        "DSLR-A390" => "DSLR-A380",
        "ILCE-3500" => "ILCE-3000",
        m => m,
    }
}

/// Visible area (margins and size) for a raw buffer of a given model.
fn visible_area(model: &str, raw_w: u32, raw_h: u32, payload: &Payload) -> Rect {
    let (mut w, mut h) = (raw_w, raw_h);
    let is = |m: &[&str]| m.contains(&model);
    if let Payload::Arq = payload {
        w = raw_w - if raw_w > 12000 { 64 } else { 32 };
        return Rect::new(0, 0, w, h);
    }
    if is(&["ILCE-7RM5", "ILCE-7CR", "ILX-LR1"]) {
        match raw_w {
            6304 => {}
            6656 => (w, h) = (6272, 4180),
            9728 => (w, h) = (9566, 6374),
            5120 => (w, h) = (4776, 3180),
            9600 => w = raw_w - 36,
            _ => w = raw_w.saturating_sub(32),
        }
    } else if raw_w == 3984 {
        w = 3925;
    } else if raw_w == 4288 {
        w -= 32;
    } else if raw_w == 4600 {
        if model == "DSLR-A350" {
            h -= 4;
        }
    } else if raw_w == 4928 {
        if h < 3280 {
            w -= 8;
        }
    } else if raw_w == 5504 {
        w -= if h > 3664 { 8 } else { 32 };
    } else if model == "ILCE-9M3" {
        match raw_w {
            6048 => w -= 36,
            6144 => (w, h) = (6012, 4020),
            4096 => (w, h) = (3944, 2644),
            3072 => (w, h) = (3016, 2008),
            3968 => w = 3948,
            _ => w = raw_w.saturating_sub(32),
        }
    } else if raw_w == 6048 {
        w -= 24;
        if model.contains("RX1") || model.contains("A99") {
            w -= 6;
        }
    } else if raw_w == 7392 {
        w -= 30;
    } else if raw_w == 8000 || raw_w == 9600 {
        w -= 32;
    } else if model == "ZV-E1" {
        if raw_w == 4608 && raw_h == 3072 {
            (w, h) = (4256, 2846);
        }
    } else if is(&["ILCE-1", "ILCE-1M2"]) {
        match (raw_w, raw_h) {
            (8704, 6144) => (w, h) = (8660, 5784),
            (8672, _) => w -= 12,
            (6144, 4096) => (w, h) = (5636, 3768),
            (5664, _) => w -= 28,
            (5632, _) => (w, h) = (raw_w - 4, 3756),
            (4608, _) => (w, h) = (4332, 2892),
            _ => {}
        }
    } else if is(&["ILCE-7M4", "ILCE-7CM2", "ILME-FX2", "ILCE-7M5"]) {
        match (raw_w, raw_h) {
            (7168, 5120) => (w, h) = (7028, 4688),
            (4736, _) => w = 4692,
            (5120, _) => (w, h) = (4624, 3080),
            (3584, _) => (w, h) = (3516, 2344),
            (7040, _) => w -= 12,
            _ => {}
        }
    } else if is(&["ILCE-6700", "ZV-E10M2", "ILME-FX30"]) {
        if raw_w == 6656 {
            (w, h) = (6240, 4168);
        } else {
            w = raw_w.saturating_sub(32);
        }
    }
    // APS-C crop modes of full-frame bodies.
    let aps_c = ((model.starts_with("ILCE-7RM") || model == "ILCA-99M2") && (raw_w == 5216 || raw_w == 6304))
        || (model == "ILCE-7R" && (4580..5020).contains(&raw_w))
        || (model == "ILCE-7" && raw_w == 3968)
        || ((model.starts_with("ILCE-7M") || model == "ILCE-9" || model.starts_with("SLT-A99"))
            && raw_w > 3750
            && raw_w < 4120)
        || (model.starts_with("ILCE-7S") && raw_w == 2816);
    if aps_c {
        w = raw_w - 32;
    }
    Rect::new(0, 0, w.min(raw_w), h.min(raw_h))
}

/// Decrypt and parse the SR2Private directory (reached via DNGPrivateData 0xC634).
/// Returns `(tag, values-as-u16)` pairs in directory order.
fn read_sr2_private(data: &[u8], tiff: &Tiff) -> Option<Vec<(u16, Vec<u32>)>> {
    let ifd0 = tiff.ifds.first()?;
    let e = ifd0.get(tags::DNG_PRIVATE_DATA)?;
    let b = e.bytes();
    if b.len() < 4 {
        return None;
    }
    let off = tiff.order.u32([b[0], b[1], b[2], b[3]]);
    let opts = flashraw_core::tiff::TiffOptions { follow_sub_ifds: false, ..Default::default() };
    let private = flashraw_core::tiff::parse_ifd_at(data, tiff.order, off, opts).ok()?;
    let sr2_off = private.u32(stag::SR2_PRIVATE_OFFSET)? as usize;
    let sr2_len = private.u32(stag::SR2_PRIVATE_LENGTH)? as usize;
    // The key is stored as 4 UNDEFINED bytes; read them as one word in file order.
    let kb = private.get(stag::SR2_PRIVATE_KEY)?.bytes();
    let key = match kb.len() {
        4.. => tiff.order.u32([kb[0], kb[1], kb[2], kb[3]]),
        _ => return None,
    };
    if sr2_len == 0 || sr2_len >= 10_240_000 {
        return None;
    }
    let mut buf = data.get(sr2_off..sr2_off.checked_add(sr2_len)?)?.to_vec();
    crypt::decrypt(&mut buf, key);
    Some(parse_flat_ifd(&buf, sr2_off, tiff.order))
}

/// Parse a decrypted IFD whose value offsets are absolute file offsets (`base` is the
/// file offset of `buf`). Values are read as unsigned 16-bit words.
fn parse_flat_ifd(buf: &[u8], base: usize, order: ByteOrder) -> Vec<(u16, Vec<u32>)> {
    let mut out = Vec::new();
    let Ok(n) = buf.u16_at(order, 0) else { return out };
    if n > 1000 {
        return out;
    }
    for i in 0..n as usize {
        let p = 2 + i * 12;
        let (Ok(tag), Ok(typ), Ok(count), Ok(val)) =
            (buf.u16_at(order, p), buf.u16_at(order, p + 2), buf.u32_at(order, p + 4), buf.u32_at(order, p + 8))
        else {
            break;
        };
        let unit = flashraw_core::tiff::type_size(typ).max(1);
        let total = unit.saturating_mul(count as usize);
        let start = if total <= 4 {
            p + 8
        } else {
            match (val as usize).checked_sub(base) {
                Some(s) => s,
                None => continue,
            }
        };
        let vals: Vec<u32> = (0..(count as usize).min(16))
            .filter_map(|c| buf.u16_at(order, start + unit * c).ok().map(|v| v as u32))
            .collect();
        out.push((tag, vals));
    }
    out
}

/// DSLR-A100: transitional format. The raw stream (ARW1) starts at the SubIFD
/// pointer, and its geometry/WB come from an embedded Minolta MRW block.
fn open_a100<'a>(
    data: &'a [u8],
    tiff: &Tiff<'a>,
    mut meta: RawMetadata,
    opts: &OpenOptions,
) -> Result<Box<dyn RawDecoder + 'a>> {
    let ifd0 = &tiff.ifds[0];
    let off =
        ifd0.u32(tags::SUB_IFDS).ok_or_else(|| Error::invalid_metadata("A100: missing raw data pointer"))? as usize;
    let mrw = ifd0
        .get(tags::DNG_PRIVATE_DATA)
        .and_then(|e| {
            let b = e.bytes();
            (b.len() >= 4).then(|| tiff.order.u32([b[0], b[1], b[2], b[3]]) as usize)
        })
        .and_then(|o| minolta::parse_blocks(data, o).ok());
    let (raw_w, raw_h) = match mrw.as_ref().and_then(|m| m.prd.as_ref()) {
        Some(p) => (p.raw_width as u32, p.raw_height as u32),
        None => (3880, 2608),
    };
    // The stream carries one extra column; the last row is not part of the image.
    let width = raw_w + 1;
    let height = raw_h;
    opts.limits.check_dims(width, height, 1)?;
    let src = data.get(off..).ok_or_else(|| Error::corrupt("A100: raw data offset beyond file"))?;
    db_lookup(&mut meta, "arw1");
    let mut info = FrameInfo::new(width, height, 12);
    info.compression = Compression::SonyArw1;
    info.active_area = Rect::new(0, 0, width, height.saturating_sub(1));
    info.crop = info.active_area;
    info.cfa = Some(CfaPattern::GRBG);
    info.layout = SensorLayout::Bayer;
    info.orientation = file_orientation(tiff);
    let entry = table::lookup("Sony", "DSLR-A100");
    let (mut black, mut cblack, mut white) = (0, [0; 4], None);
    table::apply_levels(entry, &mut black, &mut cblack, &mut white);
    info.black_level = BlackLevel::uniform(black as f32);
    info.white_level = WhiteLevel::uniform(white.unwrap_or(4095));
    if let Some(wb) = mrw.as_ref().and_then(|m| m.wb) {
        info.white_balance = sane_wb(&wb);
    }
    info.color_matrices = table::color(entry, &meta);
    meta.previews = tiff_previews(data, 0, tiff, info.orientation);
    // Only rows below `height - 1` are stored.
    let coded_rows = raw_h as usize;
    let dec = SimpleDecoder::new(
        RawFormat::Arw,
        meta,
        info,
        Box::new(move |ctx| {
            let mut img = RawImage::new_u16(width, height, 1, &ctx.limits)?;
            ctx.profiler.time(Stage::EntropyDecode, || {
                sony_arw1::decode_par(src, img.u16_mut(), width as usize, coded_rows, height as usize - 1, ctx.parallel)
            })?;
            Ok(img)
        }),
    );
    Ok(Box::new(dec))
}
