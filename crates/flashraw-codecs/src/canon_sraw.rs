//! Canon sRAW / mRAW reconstruction: subsampled YCbCr lossless-JPEG samples to linear RGB.
//!
//! A Canon "small raw" frame is a SOF3 lossless JPEG whose first component is sampled 2x1 (4:2:2) or
//! 2x2 (4:2:0). The entropy decoder (`ljpeg`) hands over rows of MCUs; each MCU holds the luma samples of a
//! 2x1 or 2x2 pixel block followed by one Cb and one Cr sample (stored with a bias of 16384). This module
//!
//! 1. places the MCUs of the stream on the pixel grid (the stream is split into vertical slices, each slice
//!    running band by band),
//! 2. interpolates chroma to every pixel (halves round up), and
//! 3. converts to RGB with the camera-generation specific matrix and the white-balance multipliers.
//!
//! The output is `width * height` pixels of interleaved `u16` R, G, B in `0..=32767`.

use crate::ljpeg::{self, LjpegHeader};
use crate::ljpeg_par;
use flashraw_core::{Error, Result, par};

/// Colour conversion variant (depends on the camera generation).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SrawConversion {
    /// Early bodies. `subtract_512` removes a luma offset of 512 first.
    Original { subtract_512: bool },
    /// Later bodies: chroma scaled by 4 plus `hue`, full 3x3 matrix.
    Matrix,
}

/// Geometry and colour parameters of an sRAW frame.
#[derive(Debug, Clone, Copy)]
pub struct SrawParams {
    /// Output width in pixels; also the row stride of the output.
    pub width: usize,
    /// Output height in pixels.
    pub height: usize,
    /// Width of the encoded area, used to bound the last slice.
    pub raw_width: usize,
    /// CR2 slice description `[count, width, last_width]`; `[0, 0, 0]` when not sliced.
    pub slices: [u16; 3],
    pub conversion: SrawConversion,
    /// Chroma offset for the `Matrix` conversion.
    pub hue: i32,
    /// White-balance multipliers R, G, B (1024 = unity).
    pub mul: [u16; 3],
}

impl SrawParams {
    /// Hue offset for a frame with the given `sraw` value; `newer` selects the later-body rule.
    pub fn default_hue(sraw: u32, newer: bool) -> i32 {
        // Twice the value for the later bodies, four times (value + 1) for the early ones.
        let factor: u32 = if newer { 2 } else { 4 };
        let base = if newer { sraw } else { sraw.wrapping_add(1) };
        base.wrapping_mul(factor) as i32
    }
}

/// Bias stored in the unsigned chroma samples.
const CHROMA_BIAS: u16 = 16384;

/// Column range `[start, end)` of one non-empty slice, in pixel columns.
type ColRange = (usize, usize);

/// Pixel-column boundaries between consecutive slices, starting at column 0. Boundary `k + 1` is
/// the end of slice `k`: one slice width after boundary `k`, except that an unsliced frame and a
/// slice reaching the encoded width both end at the even part of that width.
fn slice_bounds(p: &SrawParams, clrs: usize) -> Vec<usize> {
    let slice_count = p.slices[0] as usize + 1;
    let slice_w = p.slices[1] as usize * 2 / clrs;
    let encoded_end = p.raw_width & !1;
    let mut bounds = Vec::with_capacity(slice_count + 1);
    bounds.push(0usize);
    for _ in 1..=slice_count {
        let prev = *bounds.last().unwrap();
        let natural = prev.saturating_add(slice_w);
        let reaches_end = p.slices[0] == 0 || natural >= p.raw_width;
        bounds.push(if reaches_end { encoded_end } else { natural });
    }
    bounds
}

/// The non-empty slices in stream order.
fn slice_ranges(p: &SrawParams, clrs: usize) -> Vec<ColRange> {
    slice_bounds(p, clrs).windows(2).map(|w| (w[0], w[1])).filter(|&(start, end)| start < end).collect()
}

/// Walks the pixel positions the successive MCUs of the stream belong to.
struct Cursor<'a> {
    slices: &'a [ColRange],
    idx: usize,
    row: usize,
    col: usize,
    band: usize,
    height: usize,
}

impl Cursor<'_> {
    #[inline(always)]
    fn advance(&mut self) {
        let (s, e) = self.slices[self.idx];
        self.col += 2;
        if self.col >= e {
            self.col = s;
            self.row += self.band;
            if self.row >= self.height {
                self.row = 0;
                self.idx += 1;
                if let Some(&(s2, _)) = self.slices.get(self.idx) {
                    self.col = s2;
                }
            }
        }
    }
}

/// Store one MCU: luma samples on the 2x1 / 2x2 / 3-pixel block anchored at pixel `base`, chroma on the
/// anchor pixel. Luma samples falling at or past pixel `limit` are dropped.
#[inline(always)]
fn put_mcu(out: &mut [u16], base: usize, w: usize, lum: usize, limit: usize, m: &[u16]) {
    for (j, &v) in m[..lum].iter().enumerate() {
        let q = base + (j >> 1) * w + (j & 1);
        if q < limit {
            out[q * 3] = v;
        }
    }
    out[base * 3 + 1] = m[lum].wrapping_sub(CHROMA_BIAS);
    out[base * 3 + 2] = m[lum + 1].wrapping_sub(CHROMA_BIAS);
}

/// Most samples one row of MCUs may hold (a 4:2:0 frame this limit allows is about 87,000 pixels
/// wide). Larger rows are rejected as corrupt before any buffer is sized from them.
const MAX_ROW_SAMPLES: usize = 1 << 18;

/// Raised from the row sink once every expected MCU is in place, to stop the entropy decoder.
const DONE_MSG: &str = "sRAW: complete";

/// Decode an sRAW stream into `out` (`width * height * 3` samples, RGB interleaved).
/// `parallel` allows multi-threaded work.
pub fn decode(data: &[u8], hdr: &LjpegHeader, p: &SrawParams, parallel: bool, out: &mut [u16]) -> Result<()> {
    let clrs = hdr.clrs as usize;
    if hdr.sraw == 0 || !(4..=6).contains(&clrs) {
        return Err(Error::corrupt("sRAW: frame is not subsampled YCbCr"));
    }
    let (w, h) = (p.width, p.height);
    let n_px = w.checked_mul(h);
    let n_samples = n_px.and_then(|n| n.checked_mul(3));
    let Some(n_samples) = n_samples.filter(|&n| n <= out.len() && w >= 2 && h >= 1) else {
        return Err(Error::corrupt("sRAW: invalid output geometry"));
    };
    let out = &mut out[..n_samples];
    out.fill(0);
    let mut jh = hdr.clone();
    jh.width = hdr.width / 2;
    let mcus_per_row = jh.width as usize;
    if mcus_per_row == 0 || mcus_per_row.saturating_mul(clrs) > MAX_ROW_SAMPLES {
        return Err(Error::corrupt("sRAW: invalid MCU row length"));
    }

    let lum = clrs - 2;
    let band = clrs / 2 - 1;
    let slices = slice_ranges(p, clrs);
    let bands = h.div_ceil(band);
    let expected: usize =
        slices.iter().fold(0usize, |a, &(s, e)| a.saturating_add((e - s).div_ceil(2).saturating_mul(bands)));

    if expected > 0 {
        place(data, &jh, &slices, expected, band, lum, w, h, parallel, out)?;
    }
    finish(out, w, h, hdr.sraw >> 1 != 0, p, parallel);
    Ok(())
}

/// Entropy-decode the stream and put its MCUs on the pixel grid.
#[allow(clippy::too_many_arguments)]
fn place(
    data: &[u8],
    jh: &LjpegHeader,
    slices: &[ColRange],
    expected: usize,
    band: usize,
    lum: usize,
    w: usize,
    h: usize,
    parallel: bool,
    out: &mut [u16],
) -> Result<()> {
    let clrs = jh.clrs as usize;
    let mpr = jh.width as usize;
    let total = w * h;
    let rows_needed = expected.div_ceil(mpr).min(jh.height as usize);
    let min = if jh.restart_interval != 0 { par::PAR_MIN_SAMPLES } else { ljpeg_par::LJPEG_PARALLEL_MIN_SAMPLES };
    if parallel && mpr * clrs * rows_needed >= min {
        let mut frame = vec![0u16; mpr * clrs * rows_needed];
        if ljpeg_par::decode_frame_rows(data, jh, rows_needed, &mut frame)? {
            place_frame(&frame, slices, band, lum, w, h, out);
            return Ok(());
        }
    }
    let mut cur = Cursor { slices, idx: 0, row: 0, col: slices[0].0, band, height: h };
    let mut remaining = expected;
    let res = ljpeg::decode_rows_par_limit(data, jh, parallel, rows_needed, |_, row| {
        let take = remaining.min(mpr);
        for m in row.chunks_exact(clrs).take(take) {
            if cur.col < w {
                put_mcu(out, cur.row * w + cur.col, w, lum, total, m);
            }
            cur.advance();
        }
        remaining -= take;
        if remaining == 0 {
            return Err(Error::corrupt(DONE_MSG));
        }
        Ok(())
    });
    match res {
        Ok(()) => Ok(()),
        // Every expected MCU is placed: whatever the stream holds after that is irrelevant.
        Err(_) if remaining == 0 => Ok(()),
        Err(e) => Err(e),
    }
}

/// Place the MCUs of a fully decoded frame (the stream is the frame's samples in order). Bands are
/// independent when no pixel can be written twice, which lets them run on several threads; otherwise one
/// chunk walks the stream in order, so later MCUs overwrite earlier ones exactly as in the streaming path.
fn place_frame(frame: &[u16], slices: &[ColRange], band: usize, lum: usize, w: usize, h: usize, out: &mut [u16]) {
    let clrs = lum + 2;
    let avail = frame.len() / clrs;
    let bands = h.div_ceil(band);
    // First stream index of every slice.
    let mut bases = Vec::with_capacity(slices.len());
    let mut acc = 0usize;
    for &(s, e) in slices {
        bases.push(acc);
        acc = acc.saturating_add((e - s).div_ceil(2).saturating_mul(bands));
    }
    let disjoint = w % 2 == 0 && slices.iter().all(|&(s, e)| s % 2 == 0 && e % 2 == 0);
    let bands_per_chunk = if disjoint { bands.div_ceil(par::threads().max(1) * 4).max(4) } else { bands };
    par::for_each_row_chunk(out, w * 3, bands_per_chunk * band, disjoint, |first_row, chunk| {
        let limit = chunk.len() / 3;
        let b0 = first_row / band;
        let b1 = (b0 + bands_per_chunk).min(bands);
        for (&(s, e), &base) in slices.iter().zip(&bases) {
            let nb = (e - s).div_ceil(2);
            for b in b0..b1 {
                let local = (b * band - first_row) * w;
                let first = base.saturating_add(b * nb);
                let count = nb.min(avail.saturating_sub(first)).min(w.saturating_sub(s).div_ceil(2));
                let mcus = frame[first.min(avail) * clrs..].chunks_exact(clrs).take(count);
                for (j, m) in mcus.enumerate() {
                    put_mcu(chunk, local + s + 2 * j, w, lum, limit, m);
                }
            }
        }
    });
}

/// Chroma interpolation and colour conversion over the whole image.
fn finish(out: &mut [u16], w: usize, h: usize, vertical: bool, p: &SrawParams, parallel: bool) {
    let rl = w * 3;
    let vertical = vertical && h > 1;
    let rows_per_chunk = if parallel { ((h / (par::threads().max(1) * 4)).max(8)) & !1 } else { (h + 1) & !1 };
    // Even-row chroma at the first row of every chunk but the first: the last odd row of the
    // preceding chunk interpolates from it.
    let mut edges: Vec<Vec<i16>> = Vec::new();
    if vertical && rows_per_chunk < h {
        let mut y = rows_per_chunk;
        while y < h {
            let row = &out[y * rl..(y + 1) * rl];
            edges.push(row.chunks_exact(3).step_by(2).flat_map(|px| [px[1] as i16, px[2] as i16]).collect());
            y += rows_per_chunk;
        }
    }
    let edges = &edges;
    par::for_each_row_chunk(out, rl, rows_per_chunk, parallel, |first, chunk| {
        let n = chunk.len() / rl;
        if vertical {
            let mut r = 1;
            while r < n {
                let y = first + r;
                let (head, tail) = chunk.split_at_mut(r * rl);
                let above = &head[(r - 1) * rl..];
                let (cur, rest) = tail.split_at_mut(rl);
                let below = if y == h - 1 { None } else { rest.get(..rl) };
                let edge = if below.is_none() && y != h - 1 { Some(&edges[first / rows_per_chunk][..]) } else { None };
                for (i, x) in (0..w).step_by(2).enumerate() {
                    for c in 1..3 {
                        let a = above[x * 3 + c] as i16 as i32;
                        cur[x * 3 + c] = if y == h - 1 {
                            a as u16
                        } else {
                            let b = match (below, edge) {
                                (Some(bl), _) => bl[x * 3 + c] as i16 as i32,
                                (None, Some(e)) => e[i * 2 + c - 1] as i32,
                                (None, None) => unreachable!(),
                            };
                            mean_rounding_up(a, b) as i16 as u16
                        };
                    }
                }
                r += 2;
            }
        }
        for row in chunk.chunks_exact_mut(rl) {
            horizontal(row, w);
            convert_row(row, p);
        }
    });
}

/// Mean of two values rounded towards +infinity: `ceil((a + b) / 2)`, written without the sum.
#[inline(always)]
fn mean_rounding_up(a: i32, b: i32) -> i32 {
    (a | b) - ((a ^ b) >> 1)
}

/// Fill the chroma of odd columns from their even neighbours.
#[inline]
fn horizontal(row: &mut [u16], w: usize) {
    for x in (1..w).step_by(2) {
        for c in 1..3 {
            let l = row[(x - 1) * 3 + c];
            row[x * 3 + c] = if x == w - 1 {
                l
            } else {
                let r = row[(x + 1) * 3 + c];
                mean_rounding_up(l as i16 as i32, r as i16 as i32) as i16 as u16
            };
        }
    }
}

#[inline(always)]
fn scale(v: i32, m: i32) -> u16 {
    (v.wrapping_mul(m) >> 10).clamp(0, 32767) as u16
}

/// Contribution of the two colour differences (`cb`, `cr`) to each of R, G, B in the `Matrix`
/// conversion, in units of 2^-14.
const MATRIX_WEIGHTS: [[i32; 2]; 3] = [[50, 22929], [-5640, -11751], [29040, -101]];
const MATRIX_FRACTION_BITS: u32 = 14;
/// Colour-difference gain of the `Matrix` conversion (the stored differences are quarter size).
const MATRIX_CHROMA_GAIN: i32 = 4;

/// Weights of (`cb`, `cr`) in the green channel of the `Original` conversion, in units of 2^-12;
/// red takes `cr` and blue takes `cb` unscaled.
const ORIGINAL_GREEN_WEIGHTS: [i32; 2] = [-778, -2048];
const ORIGINAL_FRACTION_BITS: u32 = 12;
/// Luma offset removed by the `subtract_512` variant.
const ORIGINAL_LUMA_OFFSET: i16 = 512;

#[inline(always)]
fn weighted_diff(w: [i32; 2], shift: u32, cb: i32, cr: i32) -> i32 {
    (w[0] * cb + w[1] * cr) >> shift
}

fn convert_row(row: &mut [u16], p: &SrawParams) {
    let gains = p.mul.map(i32::from);
    match p.conversion {
        SrawConversion::Matrix => {
            let hue = p.hue;
            for px in row.chunks_exact_mut(3) {
                let y = px[0] as i16 as i32;
                let cb = (MATRIX_CHROMA_GAIN * px[1] as i16 as i32).wrapping_add(hue) as i16 as i32;
                let cr = (MATRIX_CHROMA_GAIN * px[2] as i16 as i32).wrapping_add(hue) as i16 as i32;
                for (c, out) in px.iter_mut().enumerate() {
                    *out = scale(y + weighted_diff(MATRIX_WEIGHTS[c], MATRIX_FRACTION_BITS, cb, cr), gains[c]);
                }
            }
        }
        SrawConversion::Original { subtract_512 } => {
            for px in row.chunks_exact_mut(3) {
                let y0 = px[0] as i16;
                let y = if subtract_512 { y0.wrapping_sub(ORIGINAL_LUMA_OFFSET) } else { y0 } as i32;
                let cb = px[1] as i16 as i32;
                let cr = px[2] as i16 as i32;
                px[0] = scale(y + cr, gains[0]);
                px[1] = scale(y + weighted_diff(ORIGINAL_GREEN_WEIGHTS, ORIGINAL_FRACTION_BITS, cb, cr), gains[1]);
                px[2] = scale(y + cb, gains[2]);
            }
        }
    }
}

/// Deterministic synthetic sRAW streams (shared by the unit tests and the golden-hash check).
#[cfg(test)]
pub(crate) mod testgen {
    use super::*;
    use crate::ljpeg::LjpegOptions;

    pub struct Rng(pub u64);
    impl Rng {
        pub fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        pub fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
    }

    /// A lossless JPEG with three components, component 0 sampled `hs x vs`, one complete 3-bit
    /// Huffman table (symbols 0..=7, so any bit string decodes), predictor 1, then `payload`
    /// (0xFF bytes replaced).
    pub fn stream(x: u16, y: u16, hs: u8, vs: u8, bits: u8, payload: &[u8]) -> Vec<u8> {
        let mut d = vec![0xFF, 0xD8, 0xFF, 0xC4, 0, 2 + 17 + 8, 0];
        let mut counts = [0u8; 16];
        counts[2] = 8;
        d.extend(counts);
        d.extend(0..8u8);
        d.extend([0xFF, 0xC3, 0, 17, bits]);
        d.extend(y.to_be_bytes());
        d.extend(x.to_be_bytes());
        d.push(3);
        d.extend([1, hs << 4 | vs, 0, 2, 0x11, 0, 3, 0x11, 0]);
        d.extend([0xFF, 0xDA, 0, 12, 3, 1, 0, 2, 0, 3, 0, 1, 0, 0]);
        d.extend(payload.iter().map(|&b| if b == 0xFF { 0xFE } else { b }));
        d
    }

    pub struct Case {
        pub data: Vec<u8>,
        pub hdr: LjpegHeader,
        pub params: SrawParams,
    }

    /// One random case, covering odd widths, odd slice widths, mismatching geometry, all three
    /// sampling shapes, 15 and 16 bit frames, extreme hue and multipliers, short and long payloads.
    pub fn gen_case(rng: &mut Rng) -> Option<Case> {
        let (hs, vs) = match rng.below(5) {
            0 | 1 => (2, 1),
            2 | 3 => (2, 2),
            _ => (3, 1),
        };
        let (clrs, band) = match (hs, vs) {
            (2, 1) => (4usize, 1usize),
            (2, 2) => (6, 2),
            _ => (5, 1),
        };
        let w = if rng.below(4) == 0 { 2 + rng.below(70) as usize } else { 2 * (1 + rng.below(40) as usize) };
        let h = if rng.below(6) == 0 { 1 + rng.below(3) as usize } else { 1 + rng.below(30) as usize };
        let slices = match rng.below(4) {
            0 => [0, 0, 0],
            1 => [rng.below(6) as u16, 0, 0],
            _ => {
                let sw = (1 + rng.below(40) as u16) * clrs as u16 / 2 + rng.below(3) as u16;
                [rng.below(6) as u16, sw, sw]
            }
        };
        let raw_width = match rng.below(4) {
            0 => w + rng.below(8) as usize,
            1 => (w + rng.below(8) as usize).saturating_sub(4),
            _ => w + 2 * rng.below(2) as usize,
        };
        let mpr = ((w / 2) as i64 + rng.below(5) as i64 - 2).max(1) as u64;
        let mpr = if rng.below(8) != 0 { mpr.max(32u64.div_ceil(clrs as u64)) } else { mpr };
        let jrows = ((h.div_ceil(band)) as i64 + rng.below(4) as i64 - 1).max(1) as u16;
        let est = (mpr as usize * clrs * jrows as usize * 7).div_ceil(8);
        let plen = match rng.below(4) {
            0 => rng.below(est as u64 + 8) as usize,
            1 => est * 2 + 64,
            _ => est * (60 + rng.below(70) as usize) / 100 + 8,
        };
        let payload: Vec<u8> = (0..plen).map(|_| rng.next() as u8).collect();
        let bits = if rng.below(3) == 0 { 16 } else { 15 };
        let data = stream((mpr * 2 + rng.below(2)) as u16, jrows, hs, vs, bits, &payload);
        let hdr = LjpegHeader::parse(&data, LjpegOptions::default()).ok()?;
        let conversion = match rng.below(3) {
            0 => SrawConversion::Matrix,
            1 => SrawConversion::Original { subtract_512: true },
            _ => SrawConversion::Original { subtract_512: false },
        };
        let hue = match rng.below(4) {
            0 => rng.next() as i32,
            1 => rng.below(40000) as i32 - 20000,
            _ => rng.below(20) as i32,
        };
        let mul = [rng.below(65536) as u16, (900 + rng.below(300)) as u16, (rng.next() & 0x7FF) as u16];
        let mul =
            if rng.below(2) == 0 { [1000 + rng.below(1000) as u16, 1024, 1000 + rng.below(1000) as u16] } else { mul };
        Some(Case { data, hdr, params: SrawParams { width: w, height: h, raw_width, slices, conversion, hue, mul } })
    }
}

#[cfg(test)]
mod tests {
    use super::testgen::*;
    use super::*;
    use crate::ljpeg::LjpegOptions;

    fn params(conversion: SrawConversion, hue: i32, mul: [u16; 3]) -> SrawParams {
        SrawParams { width: 4, height: 1, raw_width: 4, slices: [0; 3], conversion, hue, mul }
    }

    #[test]
    fn default_hue_values() {
        assert_eq!(SrawParams::default_hue(1, false), 8);
        assert_eq!(SrawParams::default_hue(3, false), 16);
        assert_eq!(SrawParams::default_hue(1, true), 2);
        assert_eq!(SrawParams::default_hue(3, true), 6);
    }

    #[test]
    fn slice_geometry() {
        let mut p = params(SrawConversion::Matrix, 0, [0; 3]);
        p.raw_width = 3866;
        assert_eq!(slice_ranges(&p, 4), [(0, 3866)]);
        p.raw_width = 11;
        assert_eq!(slice_ranges(&p, 4), [(0, 10)]);
        // The last slice ends at the coded width; the table's own last width is not consulted.
        p.raw_width = 10;
        p.slices = [2, 8, 4];
        assert_eq!(slice_ranges(&p, 4), [(0, 4), (4, 8), (8, 10)]);
        // More slices than fit: the rest is empty.
        p.slices = [9, 8, 4];
        assert_eq!(slice_ranges(&p, 4), [(0, 4), (4, 8), (8, 10)]);
        // Odd slice width (in columns): neighbours overlap by one column.
        p.slices = [1, 6, 0];
        assert_eq!(slice_ranges(&p, 4), [(0, 3), (3, 6)]);
    }

    #[test]
    fn cursor_orders_bands_within_slices() {
        let slices = [(0usize, 4usize), (4, 6)];
        let mut c = Cursor { slices: &slices, idx: 0, row: 0, col: 0, band: 2, height: 5 };
        let mut seen = Vec::new();
        for _ in 0..(2 * 3 + 3) {
            seen.push((c.row, c.col));
            c.advance();
        }
        assert_eq!(seen, [(0, 0), (0, 2), (2, 0), (2, 2), (4, 0), (4, 2), (0, 4), (2, 4), (4, 4)]);
    }

    fn px(y: u16, cb: i16, cr: i16) -> [u16; 3] {
        [y, cb as u16, cr as u16]
    }

    #[test]
    fn horizontal_interpolation_rounds_up() {
        let mut row = [px(1, 10, -10), [2, 0, 0], px(3, 13, -13), [4, 0, 0]].concat();
        horizontal(&mut row, 4);
        assert_eq!((row[4] as i16, row[5] as i16), (12, -11));
        assert_eq!((row[10], row[11]), (13u16, -13i16 as u16));
        assert_eq!((row[0], row[3], row[6], row[9]), (1, 2, 3, 4));
    }

    #[test]
    fn matrix_and_original_conversion() {
        let unity = [1024; 3];
        let mut row = px(1000, 100, -50).to_vec();
        convert_row(&mut row, &params(SrawConversion::Matrix, 0, unity));
        assert_eq!(row, [721, 1005, 1710]);
        // Hue is added to both scaled chroma values: cb = 400 + 8, cr = -200 + 8.
        let mut row = px(1000, 100, -50).to_vec();
        convert_row(&mut row, &params(SrawConversion::Matrix, 8, unity));
        let (cb, cr) = (408i32, -192i32);
        let e = |a: i32, b: i32| (1000 + ((a * cb + b * cr) >> 14)) as u16;
        assert_eq!(row, [e(50, 22929), e(-5640, -11751), e(29040, -101)]);

        let mut row = px(1000, 100, -50).to_vec();
        convert_row(&mut row, &params(SrawConversion::Original { subtract_512: false }, 99, unity));
        assert_eq!(row, [950, 1006, 1100]);
        let mut row = px(1000, 100, -50).to_vec();
        convert_row(&mut row, &params(SrawConversion::Original { subtract_512: true }, 0, unity));
        assert_eq!(row, [438, 494, 588]);
        // White balance multiply and clamp to 15 bits; negative values clamp to zero.
        let mut row = px(1000, 100, -50).to_vec();
        convert_row(&mut row, &params(SrawConversion::Original { subtract_512: false }, 0, [2048, 512, 0]));
        assert_eq!(row, [1900, 503, 0]);
        let mut row = px(30000, 0, 5000).to_vec();
        convert_row(&mut row, &params(SrawConversion::Original { subtract_512: false }, 0, unity));
        assert_eq!(row[0], 32767);
        let mut row = px(0, 0, 0).to_vec();
        convert_row(&mut row, &params(SrawConversion::Original { subtract_512: true }, 0, unity));
        assert_eq!(row, [0, 0, 0]);
    }

    #[test]
    fn vertical_interpolation_420() {
        // 2 columns x 4 rows; chroma only on even rows' even column. Rows 1 and 3 interpolate / copy.
        let (w, h) = (2usize, 4usize);
        let mut img = vec![0u16; w * h * 3];
        for (y, v) in [(0usize, 10i16), (2, 21)] {
            img[y * w * 3 + 1] = v as u16;
            img[y * w * 3 + 2] = (-v) as u16;
        }
        let p = params(SrawConversion::Original { subtract_512: false }, 0, [1024; 3]);
        let mut probe = img.clone();
        finish(&mut probe, w, h, true, &p, false);
        // Luma is 0, so blue = cb: rows 0..3 at x=0 are 10, 16 (halves round up), 21, and the last row copies row 2.
        let blue_at = |y: usize, x: usize| probe[(y * w + x) * 3 + 2];
        assert_eq!([blue_at(0, 0), blue_at(1, 0), blue_at(2, 0), blue_at(3, 0)], [10, 16, 21, 21]);
        assert_eq!([blue_at(0, 1), blue_at(1, 1)], [10, 16]);
    }

    #[test]
    fn rejects_non_sraw_frames() {
        let data = crate::ljpeg::encoder::encode(4, 2, 2, 12, 1, &[0; 16]);
        let hdr = LjpegHeader::parse(&data, LjpegOptions::default()).unwrap();
        let p = params(SrawConversion::Matrix, 0, [1024; 3]);
        let mut out = [0u16; 12];
        assert!(decode(&data, &hdr, &p, false, &mut out).is_err());
    }

    #[test]
    fn invalid_geometry_is_rejected() {
        let data = stream(32, 4, 2, 1, 15, &[0x55; 400]);
        let hdr = LjpegHeader::parse(&data, LjpegOptions::default()).unwrap();
        let mut p = params(SrawConversion::Matrix, 0, [1024; 3]);
        p.width = 32;
        p.height = 4;
        p.raw_width = 32;
        let mut small = vec![0u16; 32 * 4 * 3 - 1];
        assert!(decode(&data, &hdr, &p, false, &mut small).is_err());
        let mut out = vec![0u16; 32 * 4 * 3 + 7];
        out[32 * 4 * 3 + 3] = 77;
        decode(&data, &hdr, &p, false, &mut out).unwrap();
        assert_eq!(out[32 * 4 * 3 + 3], 77, "tail beyond the image is left alone");
        p.width = 1;
        assert!(decode(&data, &hdr, &p, false, &mut out).is_err());
        p.width = 32;
        p.height = 0;
        assert!(decode(&data, &hdr, &p, false, &mut out).is_err());
        // Not even one MCU per row.
        let empty_rows = stream(1, 4, 2, 1, 15, &[0x55; 400]);
        let hdr = LjpegHeader::parse(&empty_rows, LjpegOptions::default()).unwrap();
        p.height = 4;
        assert!(decode(&empty_rows, &hdr, &p, false, &mut out).is_err());
        // A short MCU row is fine as long as it holds one MCU.
        let short = stream(8, 4, 2, 1, 15, &[0x55; 400]);
        let hdr = LjpegHeader::parse(&short, LjpegOptions::default()).unwrap();
        assert!(decode(&short, &hdr, &p, false, &mut out).is_ok());
    }

    /// Reference placement: walk the stream with the cursor, one MCU at a time.
    fn place_reference(frame: &[u16], slices: &[ColRange], band: usize, lum: usize, w: usize, h: usize) -> Vec<u16> {
        let clrs = lum + 2;
        let bands = h.div_ceil(band);
        let n: usize = slices.iter().map(|&(s, e)| (e - s).div_ceil(2) * bands).sum();
        let mut out = vec![0u16; w * h * 3];
        let mut cur = Cursor { slices, idx: 0, row: 0, col: slices[0].0, band, height: h };
        for m in frame.chunks_exact(clrs).take(n) {
            if cur.col < w {
                put_mcu(&mut out, cur.row * w + cur.col, w, lum, w * h, m);
            }
            cur.advance();
        }
        out
    }

    #[test]
    fn frame_placement_matches_streaming_placement() {
        let mut rng = Rng(7);
        for i in 0..400 {
            let (clrs, band) = [(4usize, 1usize), (6, 2), (5, 1)][i % 3];
            let w = if i % 5 == 0 { 3 + rng.below(40) as usize } else { 2 * (2 + rng.below(30) as usize) };
            let h = 1 + rng.below(40) as usize;
            let raw_width = w + rng.below(6) as usize;
            let slices = [rng.below(4) as u16, (rng.below(30) as u16) * clrs as u16 / 2 + rng.below(2) as u16, 0];
            let p = SrawParams {
                width: w,
                height: h,
                raw_width,
                slices,
                conversion: SrawConversion::Matrix,
                hue: 0,
                mul: [0; 3],
            };
            let sl = slice_ranges(&p, clrs);
            if sl.is_empty() {
                continue;
            }
            let n: usize = sl.iter().map(|&(s, e)| (e - s).div_ceil(2) * h.div_ceil(band)).sum();
            let frame: Vec<u16> = (0..(n + rng.below(4) as usize) * clrs).map(|_| rng.next() as u16).collect();
            let want = place_reference(&frame, &sl, band, clrs - 2, w, h);
            let mut got = vec![9u16; w * h * 3];
            got.fill(0);
            place_frame(&frame, &sl, band, clrs - 2, w, h, &mut got);
            assert_eq!(got, want, "case {i}");
        }
    }

    #[test]
    fn parallel_flag_does_not_change_results() {
        let mut rng = Rng(99);
        for _ in 0..300 {
            let Some(c) = gen_case(&mut rng) else { continue };
            let n = c.params.width * c.params.height * 3;
            let (mut a, mut b) = (vec![1u16; n], vec![2u16; n]);
            let ra = decode(&c.data, &c.hdr, &c.params, false, &mut a);
            let rb = decode(&c.data, &c.hdr, &c.params, true, &mut b);
            assert_eq!(ra.is_ok(), rb.is_ok());
            if ra.is_ok() {
                assert_eq!(a, b);
            }
        }
    }

    #[test]
    fn output_stays_in_15_bits_and_decode_is_repeatable() {
        let mut rng = Rng(5);
        for _ in 0..300 {
            let Some(c) = gen_case(&mut rng) else { continue };
            let n = c.params.width * c.params.height * 3;
            let (mut a, mut b) = (vec![0u16; n], vec![0xFFFFu16; n]);
            if decode(&c.data, &c.hdr, &c.params, false, &mut a).is_ok() {
                assert!(a.iter().all(|&v| v <= 32767));
                decode(&c.data, &c.hdr, &c.params, false, &mut b).unwrap();
                assert_eq!(a, b);
            }
        }
    }

    fn fnv(h: &mut u64, bytes: &[u8]) {
        for &b in bytes {
            *h = (*h ^ b as u64).wrapping_mul(0x100000001b3);
        }
    }

    /// Hash of the results (output or error kind) over a fixed set of synthetic streams. The value was
    /// computed with the previous implementation as well (differential run over 300000 cases that
    /// includes these), so it pins the exact behaviour, truncation and corruption handling included.
    #[test]
    fn golden_hash_of_synthetic_streams() {
        let mut rng = Rng(0x9E3779B97F4A7C15);
        let mut h = 0xcbf29ce484222325u64;
        let mut decoded = 0;
        for _ in 0..20_000 {
            let Some(c) = gen_case(&mut rng) else { continue };
            let n = c.params.width * c.params.height * 3;
            let mut out = vec![0x5A5Au16; n];
            match decode(&c.data, &c.hdr, &c.params, false, &mut out) {
                Ok(()) => {
                    decoded += 1;
                    fnv(&mut h, &[1]);
                    fnv(&mut h, &out.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<_>>());
                }
                Err(e) => {
                    fnv(&mut h, &[0]);
                    fnv(&mut h, e.kind().as_bytes());
                }
            }
        }
        eprintln!("golden {h:#x} decoded {decoded}");
        assert_eq!((h, decoded), (GOLDEN_HASH, GOLDEN_DECODED));
    }

    const GOLDEN_HASH: u64 = 0x777069797ecb47f7;
    const GOLDEN_DECODED: usize = 15521;
}
