//! Geometry of a sample: tile grid, the per-sample header records and the size of every subband.

use super::header::CrxHeader;
use super::quant::Margins;
use flashraw_core::{Error, Result};

/// Overlap coefficients the subbands of a tile carry on a side with a neighbouring tile:
/// `[levels - 1][tile size mod 8][2 * level + {0: high-pass, 1: low-pass}]`, level 0 being the finest.
const OVERLAP: [[[u8; 6]; 8]; 3] = [
    [
        [1, 1, 0, 0, 0, 0],
        [1, 0, 0, 0, 0, 0],
        [1, 1, 0, 0, 0, 0],
        [1, 0, 0, 0, 0, 0],
        [1, 1, 0, 0, 0, 0],
        [1, 0, 0, 0, 0, 0],
        [1, 1, 0, 0, 0, 0],
        [1, 0, 0, 0, 0, 0],
    ],
    [
        [1, 1, 1, 1, 0, 0],
        [1, 0, 1, 0, 0, 0],
        [1, 2, 2, 1, 0, 0],
        [1, 1, 1, 1, 0, 0],
        [1, 1, 1, 1, 0, 0],
        [1, 0, 1, 0, 0, 0],
        [1, 2, 2, 1, 0, 0],
        [1, 1, 1, 1, 0, 0],
    ],
    [
        [1, 1, 1, 1, 1, 1],
        [1, 0, 1, 0, 1, 0],
        [1, 2, 2, 2, 2, 1],
        [1, 1, 1, 2, 2, 1],
        [1, 1, 1, 2, 2, 1],
        [1, 0, 1, 1, 1, 1],
        [1, 2, 2, 1, 1, 1],
        [1, 1, 1, 1, 1, 1],
    ],
];

/// Which sides of a tile touch another tile.
#[derive(Clone, Copy, Default)]
pub(super) struct Sides {
    pub left: bool,
    pub right: bool,
    pub top: bool,
    pub bottom: bool,
}

/// One subband: where its stream is and what it holds.
#[derive(Clone, Default, Debug)]
pub(super) struct Band {
    /// Stream start inside the sample and length; `len == 0` means the band is empty.
    pub start: u64,
    pub len: u64,
    pub width: usize,
    pub height: usize,
    /// Quantisation parameter (version 0x100), and whether it is updated per line.
    pub q: i32,
    pub q_per_line: bool,
    /// Scale parameters of version 0x200 bands.
    pub step_mult: u32,
    pub step_base: u32,
    pub margins: Margins,
}

/// One plane of one tile ("unit").
#[derive(Default)]
pub(super) struct Plane {
    /// The coarsest/only band is coded predictively.
    pub predictive: bool,
    /// Near-lossless residual precision (0 = lossless).
    pub rounded_bits: u32,
    /// Band 0 (low-pass) followed by three detail bands per level, coarsest level first.
    pub bands: Vec<Band>,
}

pub(super) struct Tile {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
    pub sides: Sides,
    /// Start and length of the tile's quantisation map, if the tile record announces one.
    pub map: Option<(u64, u64)>,
    pub planes: Vec<Plane>,
}

pub(super) struct Layout {
    pub tiles: Vec<Tile>,
    pub plane_count: usize,
}

struct Reader<'a> {
    buf: &'a [u8],
}

impl Reader<'_> {
    fn bytes<const N: usize>(&self, at: usize) -> Result<[u8; N]> {
        at.checked_add(N)
            .and_then(|e| self.buf.get(at..e))
            .and_then(|s| s.try_into().ok())
            .ok_or_else(|| Error::corrupt("CRX: truncated image header"))
    }
    fn u8(&self, at: usize) -> Result<u8> {
        Ok(self.bytes::<1>(at)?[0])
    }
    fn u16(&self, at: usize) -> Result<u16> {
        Ok(u16::from_be_bytes(self.bytes(at)?))
    }
    fn u32(&self, at: usize) -> Result<u32> {
        Ok(u32::from_be_bytes(self.bytes(at)?))
    }
    /// Check that a record of `len` bytes starting at `at` lies inside the header.
    fn fits(&self, at: usize, len: usize) -> Result<()> {
        if at.checked_add(len).is_some_and(|e| e <= self.buf.len()) {
            Ok(())
        } else {
            Err(Error::corrupt("CRX: truncated image header"))
        }
    }
}

/// Split the plane grid into tiles. Returns `(columns, rows, tile width, tile height, plane width, plane height)`.
fn tile_grid(h: &CrxHeader) -> Result<(usize, usize, usize, usize, usize, usize)> {
    let div = if h.planes == 4 { 2 } else { 1 };
    let (pw, ph) = (h.width as usize / div, h.height as usize / div);
    let (tw, th) = (h.tile_width as usize / div, h.tile_height as usize / div);
    if tw < 22 || th < 22 || pw > 0x7fff || ph > 0x7fff {
        return Err(Error::corrupt("CRX: bad tile geometry"));
    }
    let (cols, rows) = (pw.div_ceil(tw), ph.div_ceil(th));
    if cols == 0 || rows == 0 || cols > 255 || rows > 255 || pw < tw * (cols - 1) + 22 || ph < th * (rows - 1) + 22 {
        return Err(Error::corrupt("CRX: bad tile grid"));
    }
    Ok((cols, rows, tw, th, pw, ph))
}

/// Extent of one band along one axis, and how many of its samples are overlap with the
/// neighbouring tile on the near (left/top) and far (right/bottom) side.
#[derive(Clone, Copy, Default)]
struct Span {
    len: usize,
    near: usize,
    far: usize,
}

/// What one decomposition level contributes along one axis: the high-pass and the low-pass band.
#[derive(Clone, Copy, Default)]
struct LevelSpans {
    high: Span,
    low: Span,
}

/// Spans along one axis of a tile of `n` samples. `table` is the overlap row for `n & 7`;
/// `near` / `far` say whether a neighbouring tile touches that side.
///
/// Level `k` (0 = finest) halves the axis `k + 1` times, rounding up, so its half length is
/// `ceil(n / 2^(k+1))`; the high-pass band loses one sample when the length entering the level
/// (`ceil(n / 2^k)`) is odd. The coarsest low-pass span follows the last level.
fn axis_spans(n: usize, table: &[u8; 6], levels: usize, near: bool, far: bool) -> ([LevelSpans; 3], Span) {
    let mut out = [LevelSpans::default(); 3];
    let ceil_shr = |v: usize, k: usize| (v + (1usize << k) - 1) >> k;
    for (k, slot) in out.iter_mut().enumerate().take(levels) {
        let half = ceil_shr(n, k + 1);
        let odd = ceil_shr(n, k) & 1;
        let near_add = usize::from(near);
        let (hp_far, lp_far) = if far { (table[2 * k] as usize, table[2 * k + 1] as usize) } else { (0, 0) };
        // The overlap of the high-pass band is its far part plus the near sample.
        let hp_total = hp_far + near_add;
        slot.high = Span { len: (half + hp_total).saturating_sub(odd), near: near_add, far: hp_far };
        slot.low = Span { len: half + lp_far, near: 0, far: lp_far };
    }
    let coarse_far = if far { table[2 * levels - 1] as usize } else { 0 };
    let coarse = Span { len: ceil_shr(n, levels) + coarse_far, near: 0, far: coarse_far };
    (out, coarse)
}

/// Sizes and margins of the `3 * levels + 1` bands of a tile of `w x h` plane samples.
///
/// Band 0 is the coarsest low-pass; the three bands of level `k` (finest first) are stored
/// at `3 * (levels - k)` (high-pass both ways), `- 1` (low-pass across, high-pass down) and
/// `- 2` (high-pass across, low-pass down).
fn band_shapes(levels: usize, w: usize, h: usize, s: Sides) -> Vec<Band> {
    let (xs, x_coarse) = axis_spans(w, &OVERLAP[levels - 1][w & 7], levels, s.left, s.right);
    let (ys, y_coarse) = axis_spans(h, &OVERLAP[levels - 1][h & 7], levels, s.top, s.bottom);
    let mut bands = vec![Band::default(); 3 * levels + 1];
    for k in 0..levels {
        let shift = (2 - k) as u32;
        let top = 3 * (levels - k);
        let mut put = |slot: usize, x: Span, y: Span| {
            bands[slot].width = x.len;
            bands[slot].height = y.len;
            bands[slot].margins = Margins {
                col_start: x.near as i64,
                col_end: x.far as i64,
                row_start: y.near as i64,
                row_end: y.far as i64,
                shift,
            };
        };
        put(top, xs[k].high, ys[k].high);
        put(top - 1, xs[k].low, ys[k].high);
        put(top - 2, xs[k].high, ys[k].low);
    }
    bands[0].width = x_coarse.len;
    bands[0].height = y_coarse.len;
    bands[0].margins = Margins {
        col_start: 0,
        col_end: x_coarse.far as i64,
        row_start: 0,
        row_end: y_coarse.far as i64,
        shift: 3 - levels as u32,
    };
    bands
}

/// Parse the per-sample header at the start of `data` and lay out every tile, plane and band.
pub(super) fn parse(h: &CrxHeader, data: &[u8]) -> Result<Layout> {
    let hdr_size = h.mdat_header_size as usize;
    if data.len() < hdr_size {
        return Err(Error::corrupt("CRX: sample shorter than its header"));
    }
    let hdr = Reader { buf: &data[..hdr_size] };
    let (cols, rows, tw, th, pw, ph) = tile_grid(h)?;
    let planes = h.planes as usize;
    let levels = h.levels as usize;
    let mut tiles = Vec::with_capacity(cols * rows);
    let mut pos = 0usize;
    let mut tile_total = 0u64;
    for index in 0..cols * rows {
        let (col, row) = (index % cols, index / cols);
        let width = if col + 1 == cols { pw - tw * (cols - 1) } else { tw };
        let height = if row + 1 == rows { ph - th * (rows - 1) } else { th };
        let sides = Sides { left: col > 0, right: col + 1 < cols, top: row > 0, bottom: row + 1 < rows };

        // tile record
        let (sign, size) = (hdr.u16(pos)?, hdr.u16(pos + 2)? as usize);
        let with_map = match (sign, size) {
            (0xff01, 8) => false,
            (0xff11, 8) => false,
            (0xff11, 16) => true,
            _ => return Err(Error::corrupt("CRX: bad tile record")),
        };
        hdr.fits(pos, size + 4)?;
        let tile_size = hdr.u32(pos + 4)? as u64;
        if hdr.u16(pos + 8)? as usize != index {
            return Err(Error::corrupt("CRX: tile number mismatch"));
        }
        if hdr.u16(pos + 10)? != if with_map { 0x4000 } else { 0 } {
            return Err(Error::corrupt("CRX: bad tile record tail"));
        }
        let (mut qp_size, mut extra) = (0u64, 0u64);
        if with_map {
            qp_size = hdr.u32(pos + 12)? as u64;
            extra = hdr.u16(pos + 16)? as u64;
            if hdr.u16(pos + 18)? != 0 {
                return Err(Error::corrupt("CRX: bad tile record"));
            }
        }
        pos += size + 4;
        let data_start = hdr_size as u64 + tile_total;
        tile_total += tile_size;

        let mut plane_list = Vec::with_capacity(planes);
        let mut plane_total = 0u64;
        for p in 0..planes {
            hdr.fits(pos, 12)?;
            let psign = hdr.u16(pos)?;
            if (psign != 0xff02 && psign != 0xff12) || hdr.u16(pos + 2)? != 8 {
                return Err(Error::corrupt("CRX: bad plane record"));
            }
            let plane_size = hdr.u32(pos + 4)? as u64;
            let flags = hdr.u8(pos + 8)?;
            if (hdr.u8(pos + 9)? | hdr.u8(pos + 10)? | hdr.u8(pos + 11)?) != 0 || (flags >> 4) as usize != p {
                return Err(Error::corrupt("CRX: bad plane record"));
            }
            let predictive = flags & 8 != 0;
            let rounded_bits = ((flags >> 1) & 3) as u32;
            if rounded_bits != 0 && (levels != 0 || !predictive) {
                return Err(Error::corrupt("CRX: bad plane record"));
            }
            pos += 12;
            let plane_start = data_start + qp_size + extra + plane_total;
            plane_total += plane_size;

            let mut bands = if levels == 0 {
                vec![Band { width, height, ..Band::default() }]
            } else {
                band_shapes(levels, width, height, sides)
            };
            let mut band_total = 0u64;
            for (i, band) in bands.iter_mut().enumerate() {
                let p0 = pos;
                let (bsign, bsize) = (hdr.u16(p0)?, hdr.u16(p0 + 2)? as usize);
                hdr.fits(p0, bsize + 4)?;
                let wide = match (bsign, bsize) {
                    (0xff03, 8) => false,
                    (0xff13, 16) => true,
                    _ => return Err(Error::corrupt("CRX: bad subband record")),
                };
                pos += bsize + 4;
                let band_size = hdr.u32(p0 + 4)? as i32 as i64;
                if (hdr.u8(p0 + 8)? >> 4) as usize != i {
                    return Err(Error::corrupt("CRX: subband number mismatch"));
                }
                let data_size = if !wide {
                    let w = hdr.u32(p0 + 8)?;
                    band.q_per_line = w & 0x0800_0000 != 0;
                    band.q = ((w >> 19) & 0xff) as i32;
                    band_size - (w & 0x7_ffff) as i64
                } else {
                    if hdr.u16(p0 + 8)? & 0xfff != 0 || hdr.u16(p0 + 18)? != 0 {
                        return Err(Error::corrupt("CRX: bad subband record"));
                    }
                    band.step_mult = hdr.u16(p0 + 10)? as u32;
                    band.step_base = hdr.u32(p0 + 12)?;
                    band_size - hdr.u16(p0 + 16)? as i64
                };
                if data_size < 0 {
                    return Err(Error::corrupt("CRX: negative subband size"));
                }
                band.start = plane_start.wrapping_add(band_total);
                band.len = data_size as u64;
                band_total = band_total.wrapping_add(band_size as u64);
                if band.len > 0 && band.start > data.len() as u64 {
                    return Err(Error::corrupt("CRX: subband starts beyond the end of the data"));
                }
            }
            plane_list.push(Plane { predictive, rounded_bits, bands });
        }
        tiles.push(Tile {
            x: col * tw,
            y: row * th,
            width,
            height,
            sides,
            map: with_map.then_some((data_start, qp_size)),
            planes: plane_list,
        });
    }
    Ok(Layout { tiles, plane_count: planes })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sizes and margins of the `3 * levels + 1` bands of a tile of `w x h` plane samples.
    fn old_band_shapes(levels: usize, w: usize, h: usize, s: Sides) -> Vec<Band> {
        let mut bands = vec![Band::default(); 3 * levels + 1];
        let (rx, ry) = (&OVERLAP[levels - 1][w & 7], &OVERLAP[levels - 1][h & 7]);
        let (mut bw, mut bh) = (w, h);
        for k in 0..levels {
            let (odd_w, odd_h) = (bw & 1, bh & 1);
            bw = (bw + odd_w) / 2;
            bh = (bh + odd_h) / 2;
            let (mut w0, mut w1, mut h0, mut h1) = (0usize, 0usize, 0usize, 0usize);
            let (mut cs, mut rs) = (0usize, 0usize);
            if s.right {
                w0 = rx[2 * k] as usize;
                w1 = rx[2 * k + 1] as usize;
            }
            if s.left {
                w0 += 1;
                cs = 1;
            }
            if s.bottom {
                h0 = ry[2 * k] as usize;
                h1 = ry[2 * k + 1] as usize;
            }
            if s.top {
                h0 += 1;
                rs = 1;
            }
            let shift = (2 - k) as u32;
            let top = 3 * (levels - k);
            let m = |col_start: usize, col_end: usize, row_start: usize, row_end: usize| Margins {
                col_start: col_start as i64,
                col_end: col_end as i64,
                row_start: row_start as i64,
                row_end: row_end as i64,
                shift,
            };
            let size = |a: usize, b: usize| ((a + b) as i64).max(0) as usize;
            let sized = |a: usize, add: usize, sub: usize| (a as i64 + add as i64 - sub as i64).max(0) as usize;
            // high-pass both ways
            bands[top].width = sized(bw, w0, odd_w);
            bands[top].height = sized(bh, h0, odd_h);
            bands[top].margins = m(cs, w0 - cs, rs, h0 - rs);
            // low-pass horizontally, high-pass vertically
            bands[top - 1].width = size(bw, w1);
            bands[top - 1].height = sized(bh, h0, odd_h);
            bands[top - 1].margins = m(0, w1, rs, h0 - rs);
            // high-pass horizontally, low-pass vertically
            bands[top - 2].width = sized(bw, w0, odd_w);
            bands[top - 2].height = size(bh, h1);
            bands[top - 2].margins = m(cs, w0 - cs, 0, h1);
        }
        let (ex, ey) = (
            if s.right { rx[2 * levels - 1] as usize } else { 0 },
            if s.bottom { ry[2 * levels - 1] as usize } else { 0 },
        );
        bands[0].width = bw + ex;
        bands[0].height = bh + ey;
        bands[0].margins =
            Margins { col_start: 0, col_end: ex as i64, row_start: 0, row_end: ey as i64, shift: 3 - levels as u32 };
        bands
    }

    fn key(b: &Band) -> (usize, usize, [i64; 4], u32) {
        let m = &b.margins;
        (b.width, b.height, [m.col_start, m.col_end, m.row_start, m.row_end], m.shift)
    }

    #[test]
    fn band_geometry_matches_the_stepwise_procedure() {
        let mut cases = 0u64;
        for levels in 1..=3usize {
            for side_bits in 0..16u8 {
                let s = Sides {
                    left: side_bits & 1 != 0,
                    right: side_bits & 2 != 0,
                    top: side_bits & 4 != 0,
                    bottom: side_bits & 8 != 0,
                };
                for w in (1..=200usize).chain([255, 256, 257, 1000, 1023, 1024, 1025, 4095, 4096, 4097, 0x7fff]) {
                    for h in (1..=200usize).chain([255, 256, 257, 1023, 1025, 0x7fff]) {
                        let (a, b) = (old_band_shapes(levels, w, h, s), band_shapes(levels, w, h, s));
                        assert_eq!(a.len(), b.len());
                        for (i, (x, y)) in a.iter().zip(&b).enumerate() {
                            assert_eq!(key(x), key(y), "levels {levels} {w}x{h} sides {side_bits} band {i}");
                        }
                        cases += 1;
                    }
                }
            }
        }
        assert!(cases > 400_000);
    }
}
