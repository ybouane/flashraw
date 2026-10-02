//! CRX entropy coding: adaptive Golomb–Rice codes with run-length mode.
//!
//! Every subband of every plane of every tile is an independent MSB-first bit
//! stream. A symbol is coded as a unary prefix (count of zero bits before a one)
//! followed by `k` raw bits, where `k` adapts after each symbol; prefixes of 41+
//! (23+ for quantisation parameters) escape to a fixed-width raw value. Runs of
//! values equal to the prediction context are coded with a separate adaptive run
//! length (`s` parameter, tables [`RUN_STEP`]/[`RUN_EXTRA_BITS`]).
//!
//! Lines are decoded into a buffer with one padding sample on each side; the
//! padding values left by one line are part of the context of the next line (and
//! are therefore reproduced exactly).

use super::bits::CrxBits;
use flashraw_core::{Error, Result};

/// Run length added per continuation bit for run state `s`.
const RUN_STEP: [u32; 32] = [
    1, 1, 1, 1, 2, 2, 2, 2, 4, 4, 4, 4, 8, 8, 8, 8, 0x10, 0x10, 0x20, 0x20, 0x40, 0x40, 0x80, 0x80, 0x100, 0x200,
    0x400, 0x800, 0x1000, 0x2000, 0x4000, 0x8000,
];
/// Raw bits coding the remainder of a run for run state `s`.
const RUN_EXTRA_BITS: [u32; 32] =
    [0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 9, 10, 11, 12, 13, 14, 15];

#[inline(always)]
fn bits(b: &mut CrxBits, n: u32) -> u32 {
    b.get(n)
}

#[inline(always)]
fn code(b: &mut CrxBits, k: u32, escape: u32, escape_bits: u32) -> u32 {
    b.code(k, escape, escape_bits)
}

/// Sample code word (escape after 41 zeros to a 21-bit value).
#[inline(always)]
fn sample_code(b: &mut CrxBits, k: u32) -> u32 {
    code(b, k, 41, 21)
}

/// Quantisation-parameter code word (escape after 23 zeros to an 8-bit value).
#[inline(always)]
pub(crate) fn qp_code(b: &mut CrxBits, k: u32) -> u32 {
    code(b, k, 23, 8)
}

/// Zig-zag decode: 0, 1, 2, 3, ... → 0, -1, 1, -2, ...
#[inline(always)]
pub(crate) fn unzigzag(v: u32) -> i32 {
    (-((v & 1) as i32)) ^ ((v >> 1) as i32)
}

/// Adapt the Rice parameter after coding `code` with parameter `k`; `max == 0` means unbounded.
#[inline(always)]
pub(crate) fn adapt_k(k: u32, code: u32, max: u32) -> u32 {
    let q = code >> k;
    let k = k + (q > 2) as u32 + (q > 5) as u32 - (code < ((1u32 << k) >> 1)) as u32;
    if max == 0 || k < max { k } else { max }
}

/// Edge-aware (LOCO-I style) prediction from left, top and top-left samples.
#[inline(always)]
fn predict(left: i32, top: i32, top_left: i32) -> i32 {
    let delta = top.wrapping_sub(top_left);
    let neg = delta < 0;
    let gradient = left.wrapping_add(delta);
    match (((top_left < left) ^ neg), ((left < top) ^ neg)) {
        (false, _) => gradient,
        (true, false) => left,
        (true, true) => top,
    }
}

/// How a subband's lines are coded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LineMode {
    /// Prediction from the previous line (lossless planes and the wavelet LL band).
    /// `rounded_mask > 0` selects the near-lossless variant with coarser steps.
    Predictive { rounded_mask: i32 },
    /// Wavelet detail bands: coefficients coded without inter-line prediction, with
    /// the Rice parameter seeded from the line above.
    Detail,
}

/// Line-by-line decoder of one subband bit stream.
pub(crate) struct BandDecoder<'a> {
    bits: CrxBits<'a>,
    width: usize,
    height: usize,
    line: usize,
    mode: LineMode,
    rounded_bits: u32,
    k: u32,
    s: u32,
    /// Two line buffers of `width + 2` samples (previous / current alternate).
    lines: Vec<i32>,
    /// Per-position Rice parameters of the previous line (detail bands).
    ks: Vec<i32>,
}

/// The run-length part shared by all line decoders: given that a run starts and at
/// most `left` samples remain, return the run length. `Err` on an inconsistent run.
#[inline]
fn run_length(b: &mut CrxBits, s: &mut u32, left: usize, strict_after_extra: bool) -> Result<usize> {
    let mut n = 1usize;
    while bits(b, 1) != 0 {
        n += RUN_STEP[*s as usize] as usize;
        if n > left {
            n = left;
            break;
        }
        if *s < 31 {
            *s += 1;
        }
        if n == left {
            break;
        }
    }
    if n < left {
        let extra = RUN_EXTRA_BITS[*s as usize];
        if extra != 0 {
            n += bits(b, extra) as usize;
        }
        if *s > 0 {
            *s -= 1;
        }
        if strict_after_extra && n > left {
            return Err(Error::corrupt("CRX: run past end of line"));
        }
    }
    if n > left {
        return Err(Error::corrupt("CRX: run past end of line"));
    }
    Ok(n)
}

impl<'a> BandDecoder<'a> {
    pub(crate) fn new(data: &'a [u8], width: usize, height: usize, mode: LineMode) -> Self {
        let rounded_bits = match mode {
            LineMode::Predictive { rounded_mask } if rounded_mask > 0 => {
                let mut rb = 1u32;
                if rounded_mask & !1 != 0 {
                    while rb < 31 && (rounded_mask >> rb) != 0 {
                        rb += 1;
                    }
                }
                rb
            }
            _ => 0,
        };
        BandDecoder {
            bits: CrxBits::new(data),
            width,
            height,
            line: 0,
            mode,
            rounded_bits,
            k: 0,
            s: 0,
            lines: vec![0; 2 * (width + 2)],
            ks: if mode == LineMode::Detail { vec![0; width] } else { Vec::new() },
        }
    }

    /// Decode the next line; returns its `width` samples.
    pub(crate) fn next_line(&mut self) -> Result<&[i32]> {
        if self.line >= self.height {
            return Err(Error::corrupt("CRX: subband has fewer lines than required"));
        }
        let w = self.width;
        let stride = w + 2;
        // Line 0 and even lines decode into the second buffer, odd lines into the first.
        let (a, b) = self.lines.split_at_mut(stride);
        let (prev, cur) = if self.line & 1 == 1 { (b, a) } else { (a, b) };
        let first = self.line == 0;
        match (self.mode, first) {
            (LineMode::Predictive { rounded_mask }, true) if rounded_mask > 0 => {
                top_line_rounded(&mut self.bits, &mut self.k, &mut self.s, cur, w, rounded_mask)?
            }
            (LineMode::Predictive { .. }, true) => top_line(&mut self.bits, &mut self.k, &mut self.s, cur, w)?,
            (LineMode::Predictive { rounded_mask }, false) if rounded_mask > 0 => {
                line_rounded(&mut self.bits, &mut self.k, &mut self.s, prev, cur, w, rounded_mask, self.rounded_bits)?
            }
            (LineMode::Predictive { .. }, false) => {
                line_predictive(&mut self.bits, &mut self.k, &mut self.s, prev, cur, w)?
            }
            (LineMode::Detail, true) => {
                prev[0] = 0;
                top_line_detail(&mut self.bits, &mut self.k, &mut self.s, cur, &mut self.ks, w)?
            }
            (LineMode::Detail, false) => {
                line_detail(&mut self.bits, &mut self.k, &mut self.s, prev, cur, &mut self.ks, w)?
            }
        }
        if self.bits.exhausted() {
            return Err(Error::corrupt("CRX: subband bit stream truncated"));
        }
        self.line += 1;
        Ok(&cur[1..=w])
    }

    /// Read a quantisation-parameter delta from this band's stream (C-RAW v1).
    pub(crate) fn read_qp_delta(&mut self, k: &mut u32) -> Result<i32> {
        let c = qp_code(&mut self.bits, *k);
        *k = adapt_k(*k, c, 0);
        if *k > 7 {
            return Err(Error::corrupt("CRX: invalid quantisation parameter code"));
        }
        Ok(unzigzag(c))
    }
}

/// First line of a predictive band: each sample is coded relative to its left
/// neighbour; runs repeat a non-zero left value... (zero left values start runs).
fn top_line(b: &mut CrxBits, k: &mut u32, s: &mut u32, c: &mut [i32], w: usize) -> Result<()> {
    c[0] = 0;
    let mut o = 0usize;
    let mut len = w;
    while len > 1 {
        if c[o] != 0 {
            c[o + 1] = c[o];
        } else {
            if bits(b, 1) != 0 {
                let n = run_length(b, s, len, true)?;
                len -= n;
                for _ in 0..n {
                    c[o + 1] = c[o];
                    o += 1;
                }
                if len == 0 {
                    break;
                }
            }
            c[o + 1] = 0;
        }
        let code = sample_code(b, *k);
        c[o + 1] = c[o + 1].wrapping_add(unzigzag(code));
        *k = adapt_k(*k, code, 15);
        o += 1;
        len -= 1;
    }
    if len == 1 {
        c[o + 1] = c[o];
        let code = sample_code(b, *k);
        c[o + 1] = c[o + 1].wrapping_add(unzigzag(code));
        *k = adapt_k(*k, code, 15);
        o += 1;
    }
    c[o + 1] = c[o].wrapping_add(1);
    Ok(())
}

/// Near-lossless variant of [`top_line`]: residuals are scaled by `2 * mask`.
fn top_line_rounded(b: &mut CrxBits, k: &mut u32, s: &mut u32, c: &mut [i32], w: usize, mask: i32) -> Result<()> {
    c[0] = 0;
    let mut o = 0usize;
    let mut len = w;
    let scaled = |v: i32| mask.wrapping_mul(2).wrapping_mul(v).wrapping_add(v >> 31);
    while len > 1 {
        if c[o].wrapping_abs() > mask {
            c[o + 1] = c[o];
        } else {
            if bits(b, 1) != 0 {
                let n = run_length(b, s, len, true)?;
                len -= n;
                for _ in 0..n {
                    c[o + 1] = c[o];
                    o += 1;
                }
                if len == 0 {
                    break;
                }
            }
            c[o + 1] = 0;
        }
        let code = sample_code(b, *k);
        c[o + 1] = c[o + 1].wrapping_add(scaled(unzigzag(code)));
        *k = adapt_k(*k, code, 15);
        o += 1;
        len -= 1;
    }
    if len == 1 {
        // Unlike the lossless variant the last sample is not seeded from its left
        // neighbour: it accumulates onto the (zero) buffer content.
        let code = sample_code(b, *k);
        c[o + 1] = c[o + 1].wrapping_add(scaled(unzigzag(code)));
        *k = adapt_k(*k, code, 15);
        o += 1;
    }
    c[o + 1] = c[o].wrapping_add(1);
    Ok(())
}

/// One predicted sample of [`line_predictive`] at output index `o1 + 1`, context
/// index `o0` in the previous line.
#[inline(always)]
#[allow(clippy::too_many_arguments)]
fn predictive_symbol(
    b: &mut CrxBits,
    k: &mut u32,
    p: &[i32],
    c: &mut [i32],
    o0: &mut usize,
    o1: &mut usize,
    median: bool,
    not_eol: bool,
) {
    let (i0, i1) = (*o0, *o1);
    let mut v = if median { predict(c[i1], p[i0 + 1], p[i0]) } else { p[i0 + 1] };
    let mut code = sample_code(b, *k);
    v = v.wrapping_add(unzigzag(code));
    c[i1 + 1] = v;
    if not_eol {
        let next_delta = p[i0 + 2].wrapping_sub(p[i0 + 1]).wrapping_shl(1);
        code = code.wrapping_add(next_delta.wrapping_abs() as u32) >> 1;
        *o0 += 1;
    }
    *k = adapt_k(*k, code, 15);
    *o1 += 1;
}

/// Subsequent line of a predictive band.
fn line_predictive(b: &mut CrxBits, k: &mut u32, s: &mut u32, p: &[i32], c: &mut [i32], w: usize) -> Result<()> {
    c[0] = p[1];
    let (mut o0, mut o1) = (0usize, 0usize);
    let mut len = w;
    while len > 1 {
        if c[o1] != p[o0 + 1] || c[o1] != p[o0 + 2] {
            predictive_symbol(b, k, p, c, &mut o0, &mut o1, true, true);
        } else {
            if bits(b, 1) != 0 {
                let n = run_length(b, s, len, true)?;
                len -= n;
                o0 += n;
                for _ in 0..n {
                    c[o1 + 1] = c[o1];
                    o1 += 1;
                }
            }
            if len > 0 {
                predictive_symbol(b, k, p, c, &mut o0, &mut o1, false, len > 1);
            }
        }
        // `len` may already be 0 after a run reaching the end of the line.
        len = len.wrapping_sub(1);
        if len == usize::MAX {
            break;
        }
    }
    if len == 1 {
        predictive_symbol(b, k, p, c, &mut o0, &mut o1, true, false);
    }
    c[o1 + 1] = c[o1].wrapping_add(1);
    Ok(())
}

/// One near-lossless sample at output index `o1 + 1`.
#[inline(always)]
#[allow(clippy::too_many_arguments)]
fn rounded_symbol(
    b: &mut CrxBits,
    k: &mut u32,
    p: &[i32],
    c: &mut [i32],
    o0: usize,
    o1: &mut usize,
    do_sym: bool,
    do_code: bool,
    mask: i32,
    rbits: u32,
) {
    let i1 = *o1;
    let sym = if do_sym { predict(c[i1], p[o0 + 1], p[o0]) } else { p[o0 + 1] };
    let code = sample_code(b, *k);
    let v = unzigzag(code);
    c[i1 + 1] = mask.wrapping_mul(2).wrapping_mul(v).wrapping_add(v >> 31).wrapping_add(sym);
    if do_code {
        let (a, n) = (p[o0 + 2], p[o0 + 1]);
        let d = if a > n {
            a.wrapping_sub(n).wrapping_add(mask - 1) >> rbits
        } else {
            -(n.wrapping_sub(a).wrapping_add(mask) >> rbits)
        };
        *k = adapt_k(*k, code.wrapping_add(2u32.wrapping_mul(d.wrapping_abs() as u32)) >> 1, 15);
    } else {
        *k = adapt_k(*k, code, 15);
    }
    *o1 += 1;
}

/// Subsequent line of a near-lossless predictive band.
#[allow(clippy::too_many_arguments)]
fn line_rounded(
    b: &mut CrxBits,
    k: &mut u32,
    s: &mut u32,
    p: &mut [i32],
    c: &mut [i32],
    w: usize,
    mask: i32,
    rbits: u32,
) -> Result<()> {
    let mut reached = false;
    p[0] = p[1];
    c[0] = p[1];
    let (mut o0, mut o1) = (0usize, 0usize);
    let mut len = w;
    while len > 1 {
        if p[o0 + 2].wrapping_sub(p[o0 + 1]).wrapping_abs() > mask {
            rounded_symbol(b, k, p, c, o0, &mut o1, true, true, mask, rbits);
            o0 += 1;
            reached = true;
        } else if reached || p[o0].wrapping_sub(c[o1]).wrapping_abs() > mask {
            rounded_symbol(b, k, p, c, o0, &mut o1, true, true, mask, rbits);
            o0 += 1;
            reached = false;
        } else {
            if bits(b, 1) != 0 {
                let n = run_length(b, s, len, false)?;
                len -= n;
                o0 += n;
                for _ in 0..n {
                    c[o1 + 1] = c[o1];
                    o1 += 1;
                }
            }
            if len > 1 {
                rounded_symbol(b, k, p, c, o0, &mut o1, false, true, mask, rbits);
                o0 += 1;
                reached = p[o0 + 1].wrapping_sub(p[o0]).wrapping_abs() > mask;
            } else if len == 1 {
                rounded_symbol(b, k, p, c, o0, &mut o1, false, false, mask, rbits);
            }
        }
        len = len.wrapping_sub(1);
        if len == usize::MAX {
            break;
        }
    }
    if len == 1 {
        rounded_symbol(b, k, p, c, o0, &mut o1, true, false, mask, rbits);
    }
    c[o1 + 1] = c[o1].wrapping_add(1);
    Ok(())
}

/// First line of a detail (wavelet high-pass) band.
fn top_line_detail(b: &mut CrxBits, k: &mut u32, s: &mut u32, c: &mut [i32], ks: &mut [i32], w: usize) -> Result<()> {
    c[0] = 0;
    let (mut o1, mut o2) = (0usize, 0usize);
    let mut len = w;
    while len > 1 {
        if c[o1] != 0 {
            let code = sample_code(b, *k);
            c[o1 + 1] = unzigzag(code);
            *k = adapt_k(*k, code, 15);
        } else {
            let mut n = 0;
            if bits(b, 1) != 0 {
                n = run_length(b, s, len, true)?;
            }
            len -= n;
            for _ in 0..n {
                ks[o2] = 0;
                c[o1 + 1] = 0;
                o1 += 1;
                o2 += 1;
            }
            if len == 0 {
                break;
            }
            // A run is always followed by a non-zero coefficient: codes are offset by one.
            let code = sample_code(b, *k);
            c[o1 + 1] = unzigzag(code.wrapping_add(1));
            *k = adapt_k(*k, code, 15);
        }
        ks[o2] = *k as i32;
        o2 += 1;
        o1 += 1;
        len -= 1;
    }
    if len == 1 {
        let code = sample_code(b, *k);
        c[o1 + 1] = unzigzag(code);
        *k = adapt_k(*k, code, 15);
        ks[o2] = *k as i32;
        o1 += 1;
    }
    c[o1 + 1] = 0;
    Ok(())
}

/// Rice parameter update of detail lines: unbounded adaptation, then a nudge towards
/// the parameter used at the next position of the previous line.
#[inline(always)]
fn detail_k(k: u32, code: u32, above: i32) -> u32 {
    let k = adapt_k(k, code, 0);
    if above - (k as i32) <= 1 { k.min(15) } else { k + 1 }
}

/// Subsequent line of a detail band.
fn line_detail(
    b: &mut CrxBits,
    k: &mut u32,
    s: &mut u32,
    p: &[i32],
    c: &mut [i32],
    ks: &mut [i32],
    w: usize,
) -> Result<()> {
    let mut i = 0usize;
    while i + 1 < w {
        if p[i + 2] | p[i + 1] | c[i] != 0 {
            let code = sample_code(b, *k);
            c[i + 1] = unzigzag(code);
            *k = detail_k(*k, code, ks[i + 1]);
        } else {
            let mut n = 0usize;
            if bits(b, 1) != 0 {
                n = run_length(b, s, w - i, false)?;
            }
            if n > 0 {
                c[i + 1..i + 1 + n].fill(0);
                ks[i..i + n].fill(0);
                i += n;
            }
            if i + 1 >= w {
                if i + 1 == w {
                    let code = sample_code(b, *k);
                    c[i + 1] = unzigzag(code.wrapping_add(1));
                    *k = adapt_k(*k, code, 15);
                    ks[i] = *k as i32;
                }
                i += 1;
                continue;
            }
            let code = sample_code(b, *k);
            c[i + 1] = unzigzag(code.wrapping_add(1));
            *k = detail_k(*k, code, ks[i + 1]);
        }
        ks[i] = *k as i32;
        i += 1;
    }
    if i + 1 == w {
        let code = sample_code(b, *k);
        c[i + 1] = unzigzag(code);
        *k = adapt_k(*k, code, 15);
        ks[i] = *k as i32;
    }
    Ok(())
}

/// Decode the per-tile quantisation-parameter map of C-RAW v2 (`qp_w x qp_h`,
/// values already offset by +4).
pub(crate) fn decode_qp_map(data: &[u8], qp_w: usize, qp_h: usize) -> Result<Vec<i32>> {
    let mut b = CrxBits::new(data);
    let mut out = Vec::with_capacity(qp_w * qp_h);
    let stride = qp_w + 2;
    let mut lines = vec![0i32; 2 * stride];
    let mut k = 0u32;
    for row in 0..qp_h {
        let (a, bb) = lines.split_at_mut(stride);
        let (l0, l1) = if row & 1 == 1 { (bb, a) } else { (a, bb) };
        if row == 0 {
            l1[0] = 0;
            for o in 0..qp_w {
                let code = qp_code(&mut b, k);
                l1[o + 1] = l1[o].wrapping_add(unzigzag(code));
                k = adapt_k(k, code, 7);
            }
        } else {
            l1[0] = l0[1];
            let mut delta_h = l0[1].wrapping_sub(l0[0]);
            for o in 0..qp_w {
                let (left, top, top_left) = (l1[o], l0[o + 1], l0[o]);
                // Same predictor as samples, but with the horizontal gradient carried
                // from the previous position.
                let neg = delta_h < 0;
                let sel = (((top_left.wrapping_sub(left) < 0) ^ neg) as usize) << 1 | ((left < top) ^ neg) as usize;
                let pred = [left.wrapping_add(delta_h), left.wrapping_add(delta_h), left, top][sel];
                let code = qp_code(&mut b, k);
                l1[o + 1] = pred.wrapping_add(unzigzag(code));
                if o + 1 < qp_w {
                    delta_h = l0[o + 2].wrapping_sub(l0[o + 1]);
                    k = adapt_k(k, code.wrapping_add(2u32.wrapping_mul(delta_h.wrapping_abs() as u32)) >> 1, 7);
                } else {
                    k = adapt_k(k, code, 7);
                }
            }
        }
        l1[qp_w + 1] = l1[qp_w].wrapping_add(1);
        if b.exhausted() {
            return Err(Error::corrupt("CRX: quantisation map truncated"));
        }
        out.extend(l1[1..=qp_w].iter().map(|v| v.wrapping_add(4)));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal MSB-first bit writer for building synthetic streams.
    pub(crate) struct BitWriter {
        pub out: Vec<u8>,
        acc: u64,
        n: u32,
    }

    impl BitWriter {
        pub fn new() -> Self {
            BitWriter { out: Vec::new(), acc: 0, n: 0 }
        }
        pub fn put(&mut self, v: u32, bits: u32) {
            for i in (0..bits).rev() {
                self.acc = (self.acc << 1) | ((v >> i) & 1) as u64;
                self.n += 1;
                if self.n == 8 {
                    self.out.push(self.acc as u8);
                    self.acc = 0;
                    self.n = 0;
                }
            }
        }
        /// Encode a Rice code word (no escape) with parameter `k`.
        pub fn rice(&mut self, v: u32, k: u32) {
            let q = v >> k;
            self.put(0, q);
            self.put(1, 1);
            if k > 0 {
                self.put(v & ((1 << k) - 1), k);
            }
        }
        pub fn finish(mut self) -> Vec<u8> {
            if self.n > 0 {
                let n = self.n;
                self.put(0, 8 - n);
            }
            self.out.extend_from_slice(&[0; 8]);
            self.out
        }
    }

    fn zigzag(v: i32) -> u32 {
        ((v << 1) ^ (v >> 31)) as u32
    }

    #[test]
    fn zigzag_round_trip() {
        for v in -1000..1000 {
            assert_eq!(unzigzag(zigzag(v)), v);
        }
    }

    #[test]
    fn k_adaptation() {
        // Small codes shrink k, large codes grow it, clamped at max.
        assert_eq!(adapt_k(3, 0, 15), 2);
        assert_eq!(adapt_k(3, 8, 15), 3);
        assert_eq!(adapt_k(3, 3 * 8, 15), 4);
        assert_eq!(adapt_k(3, 6 * 8, 15), 5);
        assert_eq!(adapt_k(14, 1 << 22, 15), 15);
        assert_eq!(adapt_k(0, 0, 15), 0);
        assert_eq!(adapt_k(15, 1 << 25, 0), 17);
    }

    #[test]
    fn zero_counting_and_escape() {
        let mut w = BitWriter::new();
        w.put(0, 45); // 45 zeros then a one: escape
        w.put(1, 1);
        w.put(0x12345, 21);
        w.rice(77, 3);
        let data = w.finish();
        let mut b = CrxBits::new(&data);
        assert_eq!(sample_code(&mut b, 3), 0x12345);
        assert_eq!(sample_code(&mut b, 3), 77);
    }

    #[test]
    fn zeros_terminate_on_empty_stream() {
        let data = [0u8; 16];
        let mut b = CrxBits::new(&data);
        let n = b.zeros();
        assert!(n >= 128);
        assert!(b.exhausted());
    }

    #[test]
    fn predictor_cases() {
        // Smooth gradient: left + (top - top_left).
        assert_eq!(predict(10, 12, 11), 11);
        // Edge cases select left or top.
        assert_eq!(predict(10, 20, 5), 20);
        assert_eq!(predict(10, 5, 20), 5);
        assert_eq!(predict(10, 20, 30), 10);
        assert_eq!(predict(30, 20, 25), 25);
    }

    /// Encode a line with only explicit symbols (no runs) for the predictive top line:
    /// all first-line samples non-zero so no run bits are needed after the first.
    #[test]
    fn predictive_lines_decode_synthetic() {
        let w = 6usize;
        let row0 = [5i32, 7, 7, 9, 2, 4];
        let row1 = [6i32, 8, 1, 10, 3, 3];
        let mut bw = BitWriter::new();
        // Top line: first sample has left = 0 → a "no run" flag bit precedes it.
        let mut k = 0u32;
        let mut left = 0i32;
        for (i, &v) in row0.iter().enumerate() {
            if left == 0 && i + 1 < w {
                bw.put(0, 1);
            }
            let code = zigzag(v - left);
            bw.rice(code, k);
            k = adapt_k(k, code, 15);
            left = v;
        }
        // Second line: reproduce the decoder's context to pick symbols; build the
        // previous line buffer as the decoder does.
        let mut p = vec![0i32; w + 2];
        p[1..=w].copy_from_slice(&row0);
        p[w + 1] = row0[w - 1] + 1;
        let mut c = vec![0i32; w + 2];
        c[0] = p[1];
        let (mut o, mut len) = (0usize, w);
        while len > 0 {
            let in_run = len > 1 && c[o] == p[o + 1] && c[o] == p[o + 2];
            if in_run {
                bw.put(0, 1); // no run
            }
            let median = !in_run;
            let pred = if median { predict(c[o], p[o + 1], p[o]) } else { p[o + 1] };
            let v = row1[o];
            let mut code = zigzag(v - pred);
            bw.rice(code, k);
            c[o + 1] = v;
            if len > 1 {
                code = (code + ((p[o + 2] - p[o + 1]) << 1).unsigned_abs()) >> 1;
            }
            k = adapt_k(k, code, 15);
            o += 1;
            len -= 1;
        }
        let data = bw.finish();
        let mut d = BandDecoder::new(&data, w, 2, LineMode::Predictive { rounded_mask: 0 });
        assert_eq!(d.next_line().unwrap(), &row0);
        assert_eq!(d.next_line().unwrap(), &row1);
        assert!(d.next_line().is_err());
    }

    #[test]
    fn top_line_runs() {
        // Line of zeros then a value: a run covering 4 samples, then one coded sample.
        let w = 5usize;
        let mut bw = BitWriter::new();
        bw.put(1, 1); // run starts (n = 1)
        bw.put(1, 1); // +RUN_STEP[0] = 2
        bw.put(1, 1); // +RUN_STEP[1] = 3
        bw.put(1, 1); // +RUN_STEP[2] = 4 == len-1? len is 5, so continue
        bw.put(0, 1); // stop; s = 3, extra bits RUN_EXTRA_BITS[3] = 0
        // remaining len = 1 → after run, coded symbol for last sample
        bw.rice(zigzag(9), 0);
        let data = bw.finish();
        let mut d = BandDecoder::new(&data, w, 1, LineMode::Predictive { rounded_mask: 0 });
        assert_eq!(d.next_line().unwrap(), &[0, 0, 0, 0, 9]);
    }

    #[test]
    fn detail_band_truncation_is_error() {
        let data = [0u8; 4];
        let mut d = BandDecoder::new(&data, 64, 64, LineMode::Detail);
        let mut failed = false;
        for _ in 0..64 {
            if d.next_line().is_err() {
                failed = true;
                break;
            }
        }
        assert!(failed);
    }
}
