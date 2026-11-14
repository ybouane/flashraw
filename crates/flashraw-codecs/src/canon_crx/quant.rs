//! Quantisation: the step function, the per-tile quantisation-parameter map of version 0x200 files and the
//! per-column scale factors derived from it.

use super::bits::CrxBits;
use super::prim::{QP_ESCAPE, adapt, predict, unzigzag};
use flashraw_core::{Error, Result};

/// Quantiser step of the remainders 0..5 of a parameter (six steps per octave).
const STEP_BASE: [u32; 6] = [40, 45, 51, 57, 64, 72];

/// Quantiser step of parameter `q`.
#[inline]
pub(super) fn step(q: i32) -> u32 {
    let octave = q / 6;
    let rem = q % 6;
    if octave >= 6 {
        STEP_BASE[rem as usize] << ((octave - 6) as u32 & 31)
    } else {
        STEP_BASE[rem.max(0) as usize] >> ((6 - octave) as u32 & 31)
    }
}

/// A rectangular table of quantiser steps (row-major).
pub(super) struct StepTable {
    pub width: usize,
    pub height: usize,
    pub steps: Vec<u32>,
}

/// Decode the `mw x mh` quantisation-parameter map held in `stream` (values include the format's offset of 4).
fn decode_map(stream: &[u8], mw: usize, mh: usize) -> Result<Vec<i32>> {
    let mut bits = CrxBits::new(stream);
    let mut map: Vec<i32> = Vec::new();
    let mut k = 0u32;
    // raw (offset-free) values of the previous row and the left pad that goes with it
    let mut up = vec![0i32; mw];
    let mut cur = vec![0i32; mw];
    let mut up_pad = 0i32;
    let mut cur_pad;
    for y in 0..mh {
        if y == 0 {
            let mut v = 0i32;
            for slot in cur.iter_mut() {
                let c = bits.code(k, QP_ESCAPE.0, QP_ESCAPE.1);
                v = v.wrapping_add(unzigzag(c));
                k = adapt(k, c, 7);
                *slot = v;
            }
            cur_pad = 0;
        } else {
            // the pad of the row above is the first value of the row before that
            cur_pad = up[0];
            let mut left = up[0];
            for i in 0..mw {
                let tl = if i == 0 { up_pad } else { up[i - 1] };
                let c = bits.code(k, QP_ESCAPE.0, QP_ESCAPE.1);
                let v = predict(left, up[i], tl).wrapping_add(unzigzag(c));
                cur[i] = v;
                left = v;
                k = if i + 1 < mw {
                    let g = up[i + 1].wrapping_sub(up[i]).wrapping_mul(2).wrapping_abs() as u32;
                    adapt(k, c.wrapping_add(g) >> 1, 7)
                } else {
                    adapt(k, c, 7)
                };
            }
        }
        if bits.exhausted() {
            return Err(Error::corrupt("CRX: truncated quantisation map"));
        }
        map.extend(cur.iter().map(|v| v.wrapping_add(4)));
        std::mem::swap(&mut up, &mut cur);
        // The pad of row y is the first value of row y-1 (0 for the first row).
        up_pad = cur_pad;
    }
    Ok(map)
}

/// Decode a tile's map and derive the step tables of its `levels` wavelet levels (coarsest first).
pub(super) fn step_tables(stream: &[u8], tile_w: usize, tile_h: usize, levels: usize) -> Result<Vec<StepTable>> {
    if !(1..=3).contains(&levels) {
        return Err(Error::corrupt("CRX: quantisation map needs 1-3 wavelet levels"));
    }
    let mw = tile_w.div_ceil(8);
    let mh = tile_h.div_ceil(2);
    let map = decode_map(stream, mw, mh)?;
    let row = |r: usize| &map[r.min(mh - 1) * mw..][..mw];
    let mut tables = Vec::with_capacity(levels);
    if levels == 3 {
        let th = tile_h.div_ceil(8);
        let mut steps = Vec::with_capacity(mw * th);
        for r in 0..th {
            let rows = [row(4 * r), row(4 * r + 1), row(4 * r + 2), row(4 * r + 3)];
            for c in 0..mw {
                let sum = rows.iter().fold(0i32, |a, rw| a.wrapping_add(rw[c]));
                let q = sum.wrapping_add(if sum < 0 { 3 } else { 0 }) >> 2;
                steps.push(step(q));
            }
        }
        tables.push(StepTable { width: mw, height: th, steps });
    }
    if levels >= 2 {
        let th = tile_h.div_ceil(4);
        let mut steps = Vec::with_capacity(mw * th);
        for r in 0..th {
            let (a, b) = (row(2 * r), row(2 * r + 1));
            for c in 0..mw {
                steps.push(step(a[c].wrapping_add(b[c]) / 2));
            }
        }
        tables.push(StepTable { width: mw, height: th, steps });
    }
    tables.push(StepTable { width: mw, height: mh, steps: map.iter().map(|&q| step(q)).collect() });
    Ok(tables)
}

/// Where a band's columns and rows lie relative to the quantisation-map grid (version 0x200).
#[derive(Clone, Copy, Default, Debug)]
pub(super) struct Margins {
    pub col_start: i64,
    pub col_end: i64,
    pub row_start: i64,
    pub row_end: i64,
    /// Table columns cover `1 << shift` band columns.
    pub shift: u32,
}

/// Per-position scaling of one band through a step table.
pub(super) struct MapScale<'a> {
    pub table: &'a StepTable,
    pub margins: Margins,
    pub mult: u32,
    pub base: u32,
    pub band_width: usize,
    pub band_height: usize,
}

impl MapScale<'_> {
    #[inline]
    fn factor(&self, row_offset: usize, col: i64) -> i32 {
        let t =
            if col < 0 { 0 } else { self.table.steps.get(row_offset.wrapping_add(col as usize)).copied().unwrap_or(0) };
        (self.base.wrapping_add(t.wrapping_mul(self.mult) >> 3) as i32).clamp(1, 0x16_8000)
    }

    /// Scale line number `row` of the band (`src`) into `dst`.
    pub(super) fn scale_line(&self, row: usize, src: &[i32], dst: &mut [i32]) {
        let m = &self.margins;
        let (w, hb) = (self.band_width as i64, self.band_height as i64);
        let r = row as i64;
        let qr = if r < m.row_start {
            0
        } else if r < hb - m.row_end {
            r - m.row_end
        } else {
            hb - m.row_end - m.row_start - 1
        };
        let qr = qr.min(self.table.height as i64 - 1);
        let row_offset = if qr < 0 { 0 } else { self.table.width * qr as usize };
        let width = self.band_width;
        let mid_end = (w - m.col_end).clamp(0, w) as usize;
        let mid_start = m.col_start.clamp(0, mid_end as i64) as usize;
        let lead = m.col_start.clamp(0, w) as usize;
        let mul = |s: &[i32], d: &mut [i32], f: i32| {
            for (o, &v) in d.iter_mut().zip(s) {
                *o = v.wrapping_mul(f);
            }
        };
        mul(&src[..lead], &mut dst[..lead], self.factor(row_offset, 0));
        let group = 1usize << m.shift.min(16);
        let first = (mid_start as i64 - m.col_start) >> m.shift;
        let mut x = mid_start;
        let mut j = 0i64;
        while x < mid_end {
            let e = (x + group).min(mid_end);
            mul(&src[x..e], &mut dst[x..e], self.factor(row_offset, first + j));
            x = e;
            j += 1;
        }
        if mid_end < width {
            let idx = (w - m.col_end - m.col_start - 1) >> m.shift;
            mul(&src[mid_end..width], &mut dst[mid_end..width], self.factor(row_offset, idx));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_follow_six_per_octave() {
        assert_eq!(step(0), 0);
        assert_eq!(step(36), 40);
        assert_eq!(step(37), 45);
        assert_eq!(step(41), 72);
        assert_eq!(step(42), 80);
        assert_eq!(step(48), 160);
        assert_eq!(step(30), 20);
        // a negative parameter uses remainder 0 and a larger shift
        assert_eq!(step(-6), 40 >> 7);
    }
}
