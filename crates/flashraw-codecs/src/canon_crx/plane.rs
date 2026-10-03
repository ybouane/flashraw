//! Decoding of one (tile, plane) unit: subband line sources with inverse
//! quantisation, and the streaming multi-level inverse wavelet.

use super::entropy::{BandDecoder, LineMode};
use super::layout::{QStep, Subband, Tile};
use super::wavelet::{self, Sides};
use flashraw_core::{Error, Result};

/// Quantisation step for parameter `q` (6 steps per octave). Shift amounts follow x86
/// semantics (masked to 5 bits) for out-of-range parameters, which valid files never use.
pub(crate) fn q_step(q: i32) -> u32 {
    const TBL: [u32; 8] = [0x28, 0x2D, 0x33, 0x39, 0x40, 0x48, 0, 0];
    let (div, rem) = (q / 6, q % 6);
    if div >= 6 {
        TBL[rem as usize].wrapping_shl((div - 6) as u32 & 0x1f)
    } else {
        TBL[rem.max(0) as usize].wrapping_shr((6 - div) as u32)
    }
}

/// Uniform scale of C-RAW v1 bands for parameter `q`.
fn q_scale_v1(q: i32) -> i32 {
    const TBL: [i32; 8] = [0x28, 0x2D, 0x33, 0x39, 0x40, 0x48, 0, 0];
    let (div, rem) = (q / 6, q % 6);
    if div >= 6 {
        TBL[rem as usize].wrapping_mul(1i32.wrapping_shl((div + 26) as u32))
    } else {
        TBL[rem.max(0) as usize].wrapping_shr((6 - div) as u32)
    }
}

/// Produces the dequantised lines of one subband.
pub(crate) struct BandSource<'a> {
    dec: Option<BandDecoder<'a>>,
    band: &'a Subband,
    qstep: Option<&'a QStep>,
    q_param: i32,
    q_k: u32,
    line: Vec<i32>,
    /// Lines decoded so far (for the per-row quantisation table).
    row: usize,
}

impl<'a> BandSource<'a> {
    pub(crate) fn new(data: &'a [u8], band: &'a Subband, mode: LineMode, qstep: Option<&'a QStep>) -> Result<Self> {
        let dec = if band.data_size > 0 {
            let end = band.data_offset.checked_add(band.data_size).filter(|&e| e <= data.len());
            let end = end.ok_or_else(|| Error::corrupt("CRX: subband data beyond end of file"))?;
            Some(BandDecoder::new(&data[band.data_offset..end], band.width, band.height, mode))
        } else {
            None
        };
        Ok(BandSource { dec, band, qstep, q_param: band.q_param, q_k: 0, line: vec![0; band.width], row: 0 })
    }

    /// Next raw (not dequantised) line: lossless planes.
    pub(crate) fn raw_line(&mut self) -> Result<&[i32]> {
        match &mut self.dec {
            Some(d) => d.next_line(),
            None => Ok(&self.line),
        }
    }

    /// Next dequantised line (wavelet bands).
    pub(crate) fn line(&mut self) -> Result<&[i32]> {
        let Some(dec) = &mut self.dec else {
            return Ok(&self.line);
        };
        if self.band.supports_partial && self.qstep.is_none() {
            self.q_param = self.q_param.wrapping_add(dec.read_qp_delta(&mut self.q_k)?);
        }
        self.line.copy_from_slice(dec.next_line()?);
        let row = self.row;
        self.row += 1;
        match self.qstep {
            Some(q) => dequant_v2(&mut self.line, self.band, q, row),
            None => {
                let s = q_scale_v1(self.q_param);
                if s != 1 {
                    for v in self.line.iter_mut() {
                        *v = v.wrapping_mul(s);
                    }
                }
            }
        }
        Ok(&self.line)
    }
}

/// Per-position dequantisation of C-RAW v2 (quantisation map from the tile header).
fn dequant_v2(line: &mut [i32], b: &Subband, q: &QStep, row: usize) {
    let row = row as i32;
    // Row of the quantisation map covering this band line. (The middle range
    // deliberately subtracts the *end* add-on.)
    let qrow = if row < b.row_start {
        0
    } else if row < b.height as i32 - b.row_end {
        row - b.row_end
    } else {
        b.height as i32 - b.row_end - b.row_start - 1
    };
    // Valid streams stay inside the map; clamp so inconsistent headers cannot index past it.
    let qrow = qrow.min(q.height as i32 - 1);
    let base = (q.width as i64 * qrow as i64).max(0) as usize;
    let tbl = |i: i64| -> u32 {
        if i < 0 {
            return 0;
        }
        q.table.get(base + i as usize).copied().unwrap_or(0)
    };
    let scale = |t: u32| -> i32 {
        let v = (b.q_step_base as u32).wrapping_add(t.wrapping_mul(b.q_step_mult) >> 3) as i32;
        v.clamp(1, 0x168000)
    };
    let w = line.len() as i64;
    let (cs, ce) = (b.col_start as i64, b.col_end as i64);
    let shift = b.level_shift.clamp(0, 31) as u32;
    let first = scale(tbl(0));
    for v in line.iter_mut().take(cs.clamp(0, w) as usize) {
        *v = v.wrapping_mul(first);
    }
    let mid_end = (w - ce).clamp(0, w);
    let mid_start = cs.clamp(0, mid_end);
    // Each map entry covers `1 << shift` consecutive coefficients.
    let run = 1usize << shift.min(16);
    for (j, chunk) in line[mid_start as usize..mid_end as usize].chunks_mut(run).enumerate() {
        let s = scale(tbl(((mid_start - cs) >> shift) + j as i64));
        for v in chunk {
            *v = v.wrapping_mul(s);
        }
    }
    let last = scale(tbl((w - ce - cs - 1) >> shift));
    for v in line.iter_mut().skip(mid_end as usize) {
        *v = v.wrapping_mul(last);
    }
}

/// One level of the inverse wavelet, producing output rows on demand.
struct Level<'a> {
    width: usize,
    height: usize,
    h: Sides,
    v: Sides,
    /// Row buffers by role: 0 = previous even row, 1 = current high row, 2/3 = scratch.
    bufs: [Vec<i32>; 4],
    queue: [usize; 3],
    q_pos: usize,
    q_len: usize,
    emitted: usize,
    hl: BandSource<'a>,
    lh: BandSource<'a>,
    hh: BandSource<'a>,
}

/// Next output row of the last level in `levels` (the lower levels feed it).
fn pull<'s>(levels: &'s mut [Level<'_>], ll: &mut BandSource<'_>) -> Result<&'s [i32]> {
    let (lower, cur) = levels.split_at_mut(levels.len() - 1);
    let lv = &mut cur[0];
    if lv.q_pos == lv.q_len {
        produce(lv, lower, ll)?;
    }
    let slot = lv.queue[lv.q_pos];
    lv.q_pos += 1;
    Ok(&lv.bufs[slot][..lv.width])
}

/// Horizontal inverse of the next (low, HL) pair into `dst`.
fn low_row(
    lower: &mut [Level<'_>],
    ll: &mut BandSource<'_>,
    hl: &mut BandSource<'_>,
    dst: &mut [i32],
    s: Sides,
) -> Result<()> {
    let lo = if lower.is_empty() { ll.line()? } else { pull(lower, ll)? };
    let hi = hl.line()?;
    wavelet::inverse_line(lo, hi, dst, s);
    Ok(())
}

/// Horizontal inverse of the next (LH, HH) pair into `dst`.
fn high_row(lh: &mut BandSource<'_>, hh: &mut BandSource<'_>, dst: &mut [i32], s: Sides) -> Result<()> {
    let lo = lh.line()?;
    let hi = hh.line()?;
    wavelet::inverse_line(lo, hi, dst, s);
    Ok(())
}

fn produce(lv: &mut Level<'_>, lower: &mut [Level<'_>], ll: &mut BandSource<'_>) -> Result<()> {
    let (w, hs) = (lv.width, lv.h);
    let rows_left = lv.height - lv.emitted;
    if rows_left == 0 {
        return Err(Error::corrupt("CRX: wavelet level exhausted"));
    }
    let [e, h, f1, f2] = &mut lv.bufs;
    let (e, h, f1, f2) = (&mut e[..w], &mut h[..w], &mut f1[..w], &mut f2[..w]);
    let n = if lv.emitted == 0 {
        low_row(lower, ll, &mut lv.hl, e, hs)?;
        if lv.height > 1 {
            if lv.v.before {
                high_row(&mut lv.lh, &mut lv.hh, f1, hs)?;
                high_row(&mut lv.lh, &mut lv.hh, h, hs)?;
                wavelet::first_even(e, Some(f1), h);
            } else {
                high_row(&mut lv.lh, &mut lv.hh, h, hs)?;
                wavelet::first_even(e, None, h);
            }
        }
        lv.queue[0] = 0;
        1
    } else if !lv.v.after && rows_left == 1 {
        wavelet::step_last_even(e, h);
        lv.queue[0] = 1;
        1
    } else if !lv.v.after && rows_left == 2 {
        low_row(lower, ll, &mut lv.hl, f1, hs)?;
        wavelet::step_last_odd(e, h, f1);
        lv.queue[..2].copy_from_slice(&[1, 2]);
        2
    } else {
        low_row(lower, ll, &mut lv.hl, f1, hs)?;
        high_row(&mut lv.lh, &mut lv.hh, f2, hs)?;
        wavelet::step(e, h, f1, f2);
        // Rotate roles: new even = f1, new high = f2; the odd row (old high) and the
        // new even row are emitted from their new slots 3 and 0.
        lv.bufs.swap(0, 2); // slot0 = new even, slot2 = old even
        lv.bufs.swap(1, 3); // slot1 = new high, slot3 = odd row
        lv.queue[0] = 3;
        lv.queue[1] = 0;
        if rows_left >= 2 { 2 } else { 1 }
    };
    lv.q_pos = 0;
    lv.q_len = n;
    lv.emitted += n;
    Ok(())
}

/// Decode one plane of one tile, handing each output row (tile width samples) to `sink`.
pub(crate) fn decode_plane(
    data: &[u8],
    tile: &Tile,
    plane: usize,
    levels: usize,
    mut sink: impl FnMut(usize, &[i32]),
) -> Result<()> {
    let p = tile.planes.get(plane).ok_or_else(|| Error::corrupt("CRX: missing plane"))?;
    let bands = &p.bands;
    let qstep = |l: usize| tile.qstep.as_ref().and_then(|q| q.get(l));
    if levels == 0 {
        let band = bands.first().ok_or_else(|| Error::corrupt("CRX: missing subband"))?;
        if band.data_size == 0 {
            // An empty band carries no data: the tile stays zero.
            return Ok(());
        }
        let mode =
            if p.supports_partial { LineMode::Predictive { rounded_mask: p.rounded_mask } } else { LineMode::Detail };
        let mut src = BandSource::new(data, band, mode, None)?;
        for y in 0..tile.height {
            let line = src.raw_line()?;
            if line.len() < tile.width {
                return Err(Error::corrupt("CRX: plane narrower than tile"));
            }
            sink(y, &line[..tile.width]);
        }
        return Ok(());
    }
    let ll_mode =
        if p.supports_partial { LineMode::Predictive { rounded_mask: p.rounded_mask } } else { LineMode::Detail };
    let mut ll = BandSource::new(data, &bands[0], ll_mode, qstep(0))?;
    let mut lv = Vec::with_capacity(levels);
    let hs = Sides { before: tile.flags.left, after: tile.flags.right };
    let vs = Sides { before: tile.flags.top, after: tile.flags.bottom };
    for l in 0..levels {
        let (width, height) = if l + 1 == levels {
            (tile.width, tile.height)
        } else {
            (bands[3 * (l + 1) + 2].width, bands[3 * (l + 1) + 1].height)
        };
        // The lifting reads a fixed number of coefficients per line; make sure every
        // input line is long enough (inconsistent headers are rejected, not indexed).
        let (lo_need, hi_need) = wavelet::coeffs_needed(width, hs);
        let low_width = if l == 0 { bands[0].width } else { lv.last().map_or(0, |p: &Level| p.width) };
        if width == 0
            || height == 0
            || low_width < lo_need
            || bands[3 * l + 2].width < lo_need
            || bands[3 * l + 1].width < hi_need
            || bands[3 * l + 3].width < hi_need
        {
            return Err(Error::corrupt("CRX: inconsistent subband sizes"));
        }
        lv.push(Level {
            width,
            height,
            h: hs,
            v: vs,
            bufs: std::array::from_fn(|_| vec![0; width]),
            queue: [0; 3],
            q_pos: 0,
            q_len: 0,
            emitted: 0,
            hl: BandSource::new(data, &bands[3 * l + 1], LineMode::Detail, qstep(l))?,
            lh: BandSource::new(data, &bands[3 * l + 2], LineMode::Detail, qstep(l))?,
            hh: BandSource::new(data, &bands[3 * l + 3], LineMode::Detail, qstep(l))?,
        });
    }
    for y in 0..tile.height {
        let row = pull(&mut lv, &mut ll)?;
        sink(y, &row[..tile.width]);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn q_steps() {
        assert_eq!(q_step(36), 0x28);
        assert_eq!(q_step(37), 0x2D);
        assert_eq!(q_step(42), 0x50);
        assert_eq!(q_step(30), 0x28 >> 1);
        assert_eq!(q_step(4), 0x40 >> 6);
        assert_eq!(q_scale_v1(4), 1);
        assert_eq!(q_scale_v1(30), 0x14);
    }
}
