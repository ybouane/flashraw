//! Payload layouts of a Sony raw IFD and how each is read into a sensor buffer.
//!
//! The container reader (`mod.rs`) classifies the raw IFD into a [`Payload`] and builds the
//! frame description; this file turns a payload into a decoder object and reads the pixels.

use crate::common::*;
use flashraw_codecs::{ljpeg, packed, sony_arw1, sony_arw2, sony_ycc};
use flashraw_core::par::GridWriter;
use flashraw_core::tiff::{Ifd, tags};
use flashraw_core::*;

/// Payload layout of the sensor image.
#[derive(Debug, Clone)]
pub(super) enum Payload {
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

/// Whole-payload range for strip-based payloads (empty for tiles).
pub(super) fn payload_bytes<'a>(data: &'a [u8], raw: &Ifd, payload: &Payload) -> Result<PayloadRange<'a>> {
    match payload {
        Payload::LjpegTiles { .. } | Payload::Ycbcr { .. } => {
            Ok(PayloadRange { offset: 0, declared: 0, bytes: &[], unit: DataUnit::Payload })
        }
        _ => contiguous_payload(data, raw),
    }
}

/// Assemble the decoder object for a parsed payload.
pub(super) fn build_decoder<'a>(
    data: &'a [u8],
    meta: RawMetadata,
    info: FrameInfo,
    payload: Payload,
    src: PayloadRange<'a>,
    width: u32,
    height: u32,
) -> Result<Box<dyn RawDecoder + 'a>> {
    // Bytes a GPU unpack may use: what the geometry needs, as far as the file has them
    // (a truncated payload is not complete and decodes on the CPU).
    let gpu_bytes = |need: usize| data.get(src.offset..).map(|s| &s[..need.min(s.len())]).unwrap_or(&[]);
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
                data: gpu_bytes(width as usize * height as usize * 2),
                width,
                height,
                bits: 16,
                order: if *big_endian { PackOrder::MsbBytes } else { PackOrder::LsbBytes },
                row_stride: width as usize * 2,
            });
        }
        Payload::Packed12 { lsb } => {
            dec.packed = Some(PackedPayload {
                data: gpu_bytes(width as usize * 3 / 2 * height as usize),
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
fn decode_region<'a>(
    data: &'a [u8],
    src: PayloadRange<'a>,
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
            // Fixed-size rows of `w` bytes (32-pixel blocks of 16 bytes): only whole rows
            // decode; rows missing from truncated data stay zero (recover).
            let sb = src.require(data, ctx, w * region.bottom() as usize)?;
            let (truncation, src) = (sb.truncation, sb.bytes);
            let (x0, rw) = (region.x as usize, region.width as usize);
            ctx.profiler.time(Stage::EntropyDecode, || {
                par::for_each_row_chunk(img.u16_mut(), rw, 16, ctx.parallel, |first, chunk| {
                    let mut tmp = if rw == w { Vec::new() } else { vec![0u16; w] };
                    for (i, row) in chunk.chunks_mut(rw).enumerate() {
                        let y = region.y as usize + first + i;
                        let Some(s) = src.get(y * w..(y + 1) * w) else { break };
                        if tmp.is_empty() {
                            sony_arw2::decode_row(s, row, curve);
                        } else {
                            sony_arw2::decode_row(s, &mut tmp, curve);
                            row.copy_from_slice(&tmp[x0..x0 + rw]);
                        }
                    }
                });
            });
            if let Some(t) = truncation {
                let rows = (src.len() / w.max(1)).saturating_sub(region.y as usize).min(region.height as usize);
                ctx.recovered(CorruptReason::Truncated(t), ((region.height as usize - rows) * rw) as u64);
            }
            Ok(img)
        }
        Payload::Unpacked16 { big_endian } => {
            let order = if *big_endian { PackOrder::MsbBytes } else { PackOrder::LsbBytes };
            unpack_rows(data, src, w, 16, w * 2, order, region, 1, ctx)
        }
        Payload::Packed12 { lsb } => {
            let order = if *lsb { PackOrder::LsbBytes } else { PackOrder::MsbBytes };
            unpack_rows(data, src, w, 12, w * 3 / 2, order, region, 1, ctx)
        }
        Payload::Arq => {
            // Stored R, G, G2, B per pixel; the output order is R, G, B, G2.
            let geometry = RowGeometry::tight(region.bottom() as usize, w * 4, 16);
            let sb = src.require(data, ctx, geometry.needed())?;
            let (truncation, src) = (sb.truncation, sb.bytes);
            let filled = std::sync::atomic::AtomicU64::new(0);
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
                            let start = (y * w * 8 + region.x as usize * 8).min(src.len());
                            let s = &src[start..(start + rl * 2).min(src.len())];
                            // Pixels missing from truncated data (recover) stay zero.
                            let lost = (rl - s.len() / 8 * 4) as u64;
                            if lost > 0 {
                                filled.fetch_add(lost, std::sync::atomic::Ordering::Relaxed);
                            }
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
            if let Some(t) = truncation {
                ctx.recovered(CorruptReason::Truncated(t), filled.into_inner());
            }
            Ok(img)
        }
        Payload::Arw1 { coded_rows, stored_rows } => {
            let mut img = RawImage::new_u16(width, height, 1, &ctx.limits)?;
            ctx.profiler
                .time(Stage::EntropyDecode, || {
                    sony_arw1::decode_par(src.bytes, img.u16_mut(), w, *coded_rows, *stored_rows, ctx.parallel)
                })
                .map_err(|e| e.or_truncated(data.len(), src.unit, src.offset, src.declared))?;
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
            ctx.profiler
                .time(Stage::EntropyDecode, || {
                    flashraw_codecs::sony_arw6::decode(src.bytes, img.u16_mut(), w, height as usize, ctx.parallel)
                })
                .map_err(|e| e.or_truncated(data.len(), src.unit, src.offset, src.declared))?;
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

/// Unpack fixed-size rows of `samples` values (row-parallel), restricted to `region`.
/// Rows missing from truncated data fail (strict) or keep their complete samples and
/// are zero-filled with a warning (recover).
#[allow(clippy::too_many_arguments)]
fn unpack_rows(
    data: &[u8],
    src: PayloadRange<'_>,
    samples: usize,
    bits: u32,
    stride: usize,
    order: PackOrder,
    region: Rect,
    cpp: usize,
    ctx: &DecodeContext,
) -> Result<RawImage> {
    let geometry = RowGeometry::packed(region.bottom() as usize, samples, bits, stride);
    let row_bytes = geometry.row_bytes;
    let sb = src.require(data, ctx, geometry.needed())?;
    let (truncation, src) = (sb.truncation, sb.bytes);
    let filled = std::sync::atomic::AtomicU64::new(0);
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
                    let s = src.get(y * stride..).unwrap_or(&[]);
                    let s = &s[..row_bytes.min(s.len())];
                    if s.len() < row_bytes {
                        // Recovering truncated data: complete samples of this row only.
                        tmp.resize(samples, 0);
                        let n = packed::unpack_row_partial(s, &mut tmp, bits, order, simd);
                        row.copy_from_slice(&tmp[x0..x0 + rl]);
                        let keep = n.saturating_sub(x0).min(rl);
                        filled.fetch_add((rl - keep) as u64, std::sync::atomic::Ordering::Relaxed);
                    } else if tmp.is_empty() {
                        packed::unpack_row(s, row, bits, order, simd);
                    } else {
                        packed::unpack_row(s, &mut tmp, bits, order, simd);
                        row.copy_from_slice(&tmp[x0..x0 + rl]);
                    }
                }
            },
        );
    });
    if let Some(t) = truncation {
        ctx.recovered(CorruptReason::Truncated(t), filled.into_inner());
    }
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
        let (tw, th) = t.size();
        let t = std::cell::RefCell::new(&mut t);
        let decode = |src: &[u8]| -> Result<()> {
            let mut t = t.borrow_mut();
            let hdr = ljpeg::LjpegHeader::parse(src, ljpeg::LjpegOptions::default())?;
            if hdr.clrs != 4 {
                return Err(Error::unsupported_compression(
                    "ARW",
                    format!("lossless tile with {} components", hdr.clrs),
                ));
            }
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
        };
        let clear = || {
            let mut t = t.borrow_mut();
            (0..th).for_each(|y| t.row(y).fill(0));
        };
        let res = compressed_unit(data, ctx, DataUnit::Tile(i as u32), tiles[i], (tw * th) as u64, decode, clear);
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
        let (tw, th) = t.size();
        let t = std::cell::RefCell::new(&mut t);
        let decode = |src: &[u8]| -> Result<()> {
            let mut t = t.borrow_mut();
            let ycc = sony_ycc::decode_tile(src, tile_w as usize, tile_h as usize)?;
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
        };
        let clear = || {
            let mut t = t.borrow_mut();
            (0..th).for_each(|y| t.row(y).fill(0));
        };
        let res = compressed_unit(data, ctx, DataUnit::Tile(i as u32), tiles[i], (tw * th * 3) as u64, decode, clear);
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
pub(super) type TileGrid = (Vec<(usize, usize)>, u32, u32);

pub(super) fn tile_grid(raw: &Ifd) -> Result<TileGrid> {
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

/// Decode a bare ARW1 stream (the DSLR-A100 layout): a `width x height` buffer of which the first `stored_rows`
/// rows are filled from the `coded_rows` rows coded in `src`.
pub(super) fn decode_arw1_stream(
    src: &[u8],
    width: u32,
    height: u32,
    coded_rows: usize,
    stored_rows: usize,
    ctx: &DecodeContext,
) -> Result<RawImage> {
    let mut img = RawImage::new_u16(width, height, 1, &ctx.limits)?;
    ctx.profiler.time(Stage::EntropyDecode, || {
        sony_arw1::decode_par(src, img.u16_mut(), width as usize, coded_rows, stored_rows, ctx.parallel)
    })?;
    Ok(img)
}
