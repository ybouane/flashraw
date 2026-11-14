//! How the sensor samples are stored (uncompressed words, bit-packed rows, tiles, or the
//! Fujifilm compressed stream) and how they are turned into a buffer.

use flashraw_codecs::fuji_compressed::{ColorTile, FujiCompressed};
use flashraw_codecs::packed::{self, PackedLayout};
use flashraw_core::par;
use flashraw_core::*;
use std::sync::atomic::{AtomicU64, Ordering};

/// How the payload bytes are organised.
pub(super) enum Store<'a> {
    /// Rows of `bits`-bit samples, `stride` bytes apart. `exposed` is the row stride to
    /// report when the payload can be handed to the GPU unpacker as it is.
    Rows {
        bits: u32,
        order: PackOrder,
        stride: usize,
        exposed: Option<usize>,
    },
    /// `tiles` vertical strips stored one after another.
    Tiled {
        tiles: u32,
        order: ByteOrder,
    },
    Compressed(FujiCompressed<'a>),
}

/// Everything needed to decode the stored buffer.
pub(super) struct Source<'a> {
    pub data: &'a [u8],
    pub payload_offset: usize,
    pub declared: usize,
    pub width: u32,
    pub height: u32,
    pub store: Store<'a>,
    pub colors: ColorTile,
}

fn pack_order(order: ByteOrder) -> PackOrder {
    match order {
        ByteOrder::Little => PackOrder::LsbBytes,
        ByteOrder::Big => PackOrder::MsbBytes,
    }
}

impl<'a> Store<'a> {
    /// Plain 16-bit words whose rows are `stride` samples apart.
    pub fn words(order: ByteOrder, width: u32, stride: u32) -> Store<'a> {
        let exposed = (stride == width).then_some(stride as usize * 2);
        Store::Rows { bits: 16, order: pack_order(order), stride: stride as usize * 2, exposed }
    }

    /// Tightly packed rows of `bits`-bit samples.
    pub fn packed(width: u32, bits: u32, order: PackOrder) -> Store<'a> {
        let exposed = width as usize * bits as usize / 8;
        Store::Rows { bits, order, stride: (width as usize * bits as usize).div_ceil(8), exposed: Some(exposed) }
    }
}

impl<'a> Source<'a> {
    fn unit(&self) -> DataUnit {
        DataUnit::Payload
    }

    pub fn decode(&self, ctx: &DecodeContext) -> Result<RawImage> {
        let mut img = RawImage::new_u16(self.width, self.height, 1, &ctx.limits)?;
        match &self.store {
            Store::Compressed(c) => ctx
                .profiler
                .time(Stage::EntropyDecode, || {
                    c.decode(img.u16_mut(), self.width, self.height, &self.colors, ctx.parallel)
                })
                .map_err(|e| e.or_truncated(self.data.len(), self.unit(), self.payload_offset, self.declared))?,
            Store::Rows { bits, order, stride, .. } => {
                ctx.profiler.time(Stage::Unpack, || self.unpack_rows(ctx, *bits, *order, *stride, img.u16_mut()))?
            }
            Store::Tiled { tiles, order } => {
                ctx.profiler.time(Stage::Unpack, || self.unpack_tiles(ctx, *tiles, *order, img.u16_mut()))?
            }
        }
        Ok(img)
    }

    fn unpack_rows(
        &self,
        ctx: &DecodeContext,
        bits: u32,
        order: PackOrder,
        stride: usize,
        out: &mut [u16],
    ) -> Result<()> {
        let (w, h) = (self.width as usize, self.height as usize);
        let src =
            ctx.sensor_rows(self.data, self.unit(), self.payload_offset, RowGeometry::packed(h, w, bits, stride))?;
        let layout =
            PackedLayout { width: w, height: h, bits, order, row_stride: stride.max((w * bits as usize).div_ceil(8)) };
        packed::unpack(src, layout, out, w, ctx.parallel, ctx.simd, ctx.recover)
    }

    fn unpack_tiles(&self, ctx: &DecodeContext, tiles: u32, order: ByteOrder, out: &mut [u16]) -> Result<()> {
        let (w, h) = (self.width as usize, self.height as usize);
        let tw = w / tiles as usize;
        if tw == 0 {
            return Err(Error::corrupt("tiled RAF payload narrower than its tile count"));
        }
        let samples = tw * tiles as usize * h;
        let sb = ctx.sensor_bytes(self.data, self.unit(), self.payload_offset, samples.saturating_mul(2))?;
        let have = sb.bytes.len() / 2;
        let bytes = sb.bytes;
        par::for_each_row_chunk_bounded(out, w, par::memory_bound_workers(), ctx.parallel, |first, rows| {
            for (i, row) in rows.chunks_mut(w).enumerate() {
                let y = first + i;
                for t in 0..tiles as usize {
                    let base = (t * h + y) * tw;
                    let n = have.saturating_sub(base).min(tw);
                    if n == 0 {
                        continue;
                    }
                    let src = &bytes[base * 2..(base + n) * 2];
                    for (dst, s) in row[t * tw..t * tw + n].iter_mut().zip(src.chunks_exact(2)) {
                        *dst = order.u16([s[0], s[1]]);
                    }
                }
            }
        });
        if let Some(t) = sb.truncation {
            ctx.recovered(CorruptReason::Truncated(t), (samples - have.min(samples)) as u64);
        }
        Ok(())
    }

    pub fn decode_region(&self, ctx: &DecodeContext, region: Rect) -> Result<RawImage> {
        let Some(r) = region.intersect(&Rect::full(self.width, self.height)) else {
            return Err(Error::InvalidArgument(format!("region {region} outside image")));
        };
        match &self.store {
            Store::Compressed(c) => {
                let mut img = RawImage::new_u16(r.width, r.height, 1, &ctx.limits)?;
                ctx.profiler
                    .time(Stage::EntropyDecode, || c.decode_region(img.u16_mut(), r, &self.colors, ctx.parallel))
                    .map_err(|e| e.or_truncated(self.data.len(), self.unit(), self.payload_offset, self.declared))?;
                Ok(img)
            }
            Store::Rows { bits, order, stride, .. } => {
                ctx.profiler.time(Stage::Unpack, || self.rows_region(ctx, r, *bits, *order, *stride))
            }
            Store::Tiled { .. } => self.decode(ctx)?.crop(r),
        }
    }

    /// Only the rows of `r` are read; a row the file holds only in part keeps its complete
    /// samples.
    fn rows_region(
        &self,
        ctx: &DecodeContext,
        r: Rect,
        bits: u32,
        order: PackOrder,
        stride: usize,
    ) -> Result<RawImage> {
        let w = self.width as usize;
        let row_bytes = (w * bits as usize).div_ceil(8);
        let stride = stride.max(row_bytes);
        let geometry = RowGeometry::packed(r.bottom() as usize, w, bits, stride);
        let sb = ctx.sensor_bytes(self.data, self.unit(), self.payload_offset, geometry.needed())?;
        let mut img = RawImage::new_u16(r.width, r.height, 1, &ctx.limits)?;
        let (x0, rw, y0) = (r.x as usize, r.width as usize, r.y as usize);
        let whole_row = x0 == 0 && rw == w;
        let zeroed = AtomicU64::new(0);
        let bytes = sb.bytes;
        par::for_each_row_chunk_bounded(img.u16_mut(), rw, par::memory_bound_workers(), ctx.parallel, |first, rows| {
            let mut scratch = if whole_row { Vec::new() } else { vec![0u16; w] };
            let mut missing = 0u64;
            for (i, out) in rows.chunks_mut(rw).enumerate() {
                let start = (y0 + first + i).saturating_mul(stride);
                let Some(avail) = bytes.get(start..) else {
                    missing += rw as u64;
                    continue;
                };
                let src = &avail[..row_bytes.min(avail.len())];
                let done =
                    if src.len() == row_bytes { w } else { packed::complete_samples(src.len(), bits, order).min(w) };
                let dst: &mut [u16] = if whole_row { &mut *out } else { &mut scratch[..] };
                if done == w {
                    packed::unpack_row(src, dst, bits, order, ctx.simd);
                } else {
                    packed::unpack_row_partial(src, dst, bits, order, ctx.simd);
                    missing += (rw - (done.clamp(x0, x0 + rw) - x0)) as u64;
                }
                if !whole_row {
                    out.copy_from_slice(&scratch[x0..x0 + rw]);
                }
            }
            zeroed.fetch_add(missing, Ordering::Relaxed);
        });
        if let Some(t) = sb.truncation {
            ctx.recovered(CorruptReason::Truncated(t), zeroed.load(Ordering::Relaxed));
        }
        Ok(img)
    }
}
