//! Olympus / OM System ORF compressed sensor data (the adaptive entropy coder used since the E-series).
//!
//! The stream is an adaptive Golomb/Rice-style code of prediction residuals (LOCO-I median edge predictor on
//! the same-colour neighbours, two samples away). Even and odd columns are two independent channels with their own
//! adaptation state. Parsing the bits never depends on pixel values, so decoding is split in two stages: the entropy
//! stage turns a band of rows into residuals, the prediction stage turns residuals into samples. The stages run one
//! after the other, or as a two-thread pipeline when the caller allows it.

use flashraw_core::{Error, Result};

/// Parameters of the entropy coder, read from the makernote ImageProcessing directory (tags 0x640..=0x653).
///
/// The `u32`/`i32` fields hold the raw tag values; see the specification for their meaning. Every camera
/// seen so far stores the values in [`OlympusParams::default`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OlympusParams {
    /// Tag 0x640.
    pub extra_bits: u32,
    /// Tag 0x641.
    pub grad_shift: u32,
    /// Tag 0x642.
    pub edge_pred: i32,
    /// Tag 0x643.
    pub low_bits: u32,
    /// Tag 0x644.
    pub smooth_shift: u32,
    /// Tag 0x645.
    pub small_shift: u32,
    /// Tag 0x646.
    pub run_switch: i32,
    /// Tag 0x647.
    pub run_offset: i32,
    /// Tag 0x648.
    pub run_threshold: i32,
    /// Tag 0x649.
    pub base_offset: i32,
    /// Tag 0x650.
    pub base_threshold: i32,
    /// Tag 0x651.
    pub max_wbits: i32,
    /// Tag 0x652.
    pub max_high: u32,
    /// Tag 0x653.
    pub header_len: u32,
}

impl Default for OlympusParams {
    fn default() -> Self {
        OlympusParams {
            extra_bits: 0,
            grad_shift: 5,
            edge_pred: 0,
            low_bits: 2,
            smooth_shift: 5,
            small_shift: 4,
            run_switch: 3,
            run_offset: 2,
            run_threshold: 3,
            base_offset: 4,
            base_threshold: 7,
            max_wbits: 15,
            max_high: 12,
            header_len: 5,
        }
    }
}

impl OlympusParams {
    /// Build from the 14 tag values 0x640..=0x653 in tag order (`None` = absent, use the default).
    /// Tag 0x652 (`max_high`) is raised to at least 1; negative-capable fields take the `u32` bit pattern as `i32`.
    pub fn from_tags(t: &[Option<u32>; 14]) -> Self {
        let d = Self::default();
        let g = |i: usize, def: u32| t[i].unwrap_or(def);
        OlympusParams {
            extra_bits: g(0, d.extra_bits),
            grad_shift: g(1, d.grad_shift),
            edge_pred: g(2, d.edge_pred as u32) as i32,
            low_bits: g(3, d.low_bits),
            smooth_shift: g(4, d.smooth_shift),
            small_shift: g(5, d.small_shift),
            run_switch: g(6, d.run_switch as u32) as i32,
            run_offset: g(7, d.run_offset as u32) as i32,
            run_threshold: g(8, d.run_threshold as u32) as i32,
            base_offset: g(9, d.base_offset as u32) as i32,
            base_threshold: g(10, d.base_threshold as u32) as i32,
            max_wbits: g(11, d.max_wbits as u32) as i32,
            max_high: g(12, d.max_high).max(1),
            header_len: g(13, d.header_len),
        }
    }
}

impl OlympusParams {
    fn validate(&self) -> Result<()> {
        let ok = self.extra_bits <= 4
            && self.grad_shift <= 30
            && self.low_bits <= 2
            && self.small_shift <= 31
            && self.max_high <= 24
            && (1..=8).contains(&self.header_len)
            && (0..=15).contains(&self.max_wbits);
        if ok { Ok(()) } else { Err(Error::unsupported_compression("ORF", format!("coder parameters {self:?}"))) }
    }

    /// True when every coder field has its default value (the header length does not matter).
    fn is_standard(&self) -> bool {
        OlympusParams { header_len: self.header_len, ..Self::default() } == *self
    }
}

/// Check the stream header and return the byte offset of the first coded bit.
///
/// The header is `header_len` bytes holding a big-endian 1, then two zero bytes.
pub fn stream_start(data: &[u8], p: &OlympusParams) -> Result<usize> {
    let n = p.header_len as usize;
    if data.len() < n + 2 {
        return Err(Error::corrupt("ORF: compressed data too short"));
    }
    let num = data[..n].iter().fold(0u64, |a, &b| (a << 8) | u64::from(b));
    if num != 1 || data[n] != 0 || data[n + 1] != 0 {
        return Err(Error::corrupt("ORF: invalid compressed stream header"));
    }
    Ok(n + 2)
}

/// Decode `height` rows of `width` samples from the compressed stream `data` (starting at the stream header)
/// into `out` (row-major, stride `width`). Same as [`decode_with`] with `parallel = false`.
pub fn decode(data: &[u8], p: &OlympusParams, out: &mut [u16], width: usize, height: usize) -> Result<()> {
    decode_with(data, p, out, width, height, false)
}

/// Decode as [`decode`]; `parallel` permits (does not require) using more than the calling thread.
/// The result does not depend on `parallel`.
pub fn decode_with(
    data: &[u8],
    p: &OlympusParams,
    out: &mut [u16],
    width: usize,
    height: usize,
    parallel: bool,
) -> Result<()> {
    p.validate()?;
    let total = width.checked_mul(height);
    if total.is_none_or(|t| out.len() < t) {
        return Err(Error::InvalidArgument("ORF: output buffer too small".into()));
    }
    let start = stream_start(data, p)?;
    if width == 0 || height == 0 {
        return Ok(());
    }
    let body = &data[start..];
    let out = &mut out[..width * height];
    if p.is_standard() {
        fast::decode(body, out, width, height, parallel)
    } else {
        general::decode(body, p, out, width, height)
    }
}

/// Bits that may be consumed before a finished row counts as running past the data: the data plus 8 zero bytes.
fn bit_limit(body: &[u8]) -> u64 {
    8 * (body.len() as u64 + 8)
}

fn exhausted(row: usize, height: usize) -> Error {
    Error::corrupt(format!("ORF: compressed data ends at row {row}/{height}"))
}

/// Bit-at-a-time decoder for any valid parameter set, straight from the written description of the coder.
/// It is the only path for non-default parameters and the reference the fast path is tested against.
mod general {
    use super::*;
    use flashraw_core::bits::{BitPumpMsb, BitRead};

    #[derive(Clone, Copy, Default)]
    struct Chan {
        p: u32,
        run: i32,
        smooth: i32,
        prev: i32,
    }

    /// Read `n` bits from a pump that has at least 32 bits buffered.
    fn take(bits: &mut BitPumpMsb<'_>, n: u32) -> u32 {
        let v = bits.peek_no_fill(n);
        bits.skip_no_fill(n);
        v
    }

    /// Number of significant bits of `x` (0 for 0).
    pub(super) fn bit_length(x: u32) -> i32 {
        x.checked_ilog2().map_or(0, |l| l as i32 + 1)
    }

    /// One rung of the code-width ladder: the width grows by one for every bit the previous
    /// magnitude has beyond `threshold`, starting from `offset`, and never drops below
    /// `offset`.
    #[derive(Clone, Copy)]
    pub(super) struct Rung {
        pub(super) threshold: i32,
        pub(super) offset: i32,
    }

    impl Rung {
        pub(super) fn width(self, magnitude_bits: i32) -> i32 {
            let extra = magnitude_bits.wrapping_sub(self.threshold).max(-1);
            self.offset.wrapping_add(extra).wrapping_add(1)
        }
    }

    pub(super) fn predict(w: i32, n: i32, nw: i32, g: i32) -> i32 {
        let (lo, hi) = (w.min(n), w.max(n));
        if nw > lo && nw < hi {
            if n.wrapping_sub(nw).wrapping_abs() <= g && w.wrapping_sub(nw).wrapping_abs() <= g {
                w.wrapping_add(n) >> 1
            } else {
                w.wrapping_add(n).wrapping_sub(nw)
            }
        } else if nw >= hi {
            lo
        } else {
            hi
        }
    }

    pub(super) fn decode(body: &[u8], p: &OlympusParams, out: &mut [u16], w: usize, h: usize) -> Result<()> {
        let mut bits = BitPumpMsb::new(body);
        let mut ch = [Chan::default(); 2];
        let g = 1i32 << p.grad_shift;
        let run_rung = Rung { threshold: p.run_threshold, offset: p.run_offset };
        let base_rung = Rung { threshold: p.base_threshold, offset: p.base_offset };
        let e = p.extra_bits;
        for row in 0..h {
            for col in 0..w {
                let c = &mut ch[col & 1];
                let edge = col < 2;
                let wbits = if edge {
                    c.run = 0;
                    p.base_offset
                } else {
                    c.run = if c.p <= (1u32 << p.small_shift) { c.run.wrapping_add(1) } else { 0 };
                    let rung = if c.run >= p.run_switch { run_rung } else { base_rung };
                    rung.width(bit_length(c.p & 0xFFFF))
                }
                .min(p.max_wbits);
                if !(0..=15).contains(&wbits) {
                    return Err(Error::corrupt("ORF: invalid code width"));
                }
                let wbits = wbits as u32;

                // One refill covers the sign, the literal bits and the prefix window (at most 31 bits).
                bits.fill();
                let sign = take(&mut bits, 1);
                let lb = take(&mut bits, p.low_bits);
                let eb = take(&mut bits, p.extra_bits);
                let window = bits.peek_no_fill(p.max_high);
                let high = if window != 0 {
                    let z = window.leading_zeros() - (32 - p.max_high);
                    bits.skip_no_fill(z + 1);
                    z
                } else {
                    bits.skip_no_fill(p.max_high);
                    let hi = if wbits != 15 { bits.get(15 - wbits) } else { 0 };
                    bits.skip_no_fill(1);
                    hi
                };
                let rem = bits.get(wbits);
                let glc = rem | high.wrapping_shl(wbits);
                c.p = glc;
                let mut code = if sign == 0 { glc as i32 } else { !glc as i32 };
                let v = if edge { 0 } else { c.smooth };
                let vbits = match p.low_bits {
                    0 => 0,
                    1 => {
                        code = (lb as i32).wrapping_add(v.wrapping_add(code).wrapping_mul(2));
                        v.wrapping_add(code >> 1)
                    }
                    _ => {
                        code = (lb as i32).wrapping_add(v.wrapping_add(code).wrapping_mul(4));
                        // Three times the quotient by four: the term and its double.
                        v.wrapping_add((code >> 2).wrapping_mul(3))
                    }
                };
                c.smooth = if p.smooth_shift >= 15 { 0 } else { vbits >> p.smooth_shift };

                let west = if edge { p.edge_pred } else { c.prev };
                let (north, nw) = if row < 2 {
                    (p.edge_pred, p.edge_pred)
                } else {
                    let n = i32::from(out[(row - 2) * w + col]) >> e;
                    let nw = if col < 2 { p.edge_pred } else { i32::from(out[(row - 2) * w + col - 2]) >> e };
                    (n, nw)
                };
                let val = code.wrapping_add(predict(west, north, nw, g));
                c.prev = val;
                out[row * w + col] = (eb as i32).wrapping_add(val.wrapping_shl(e)) as u16;
            }
            if bits.exhausted() {
                return Err(exhausted(row, h));
            }
        }
        Ok(())
    }
}

/// The default-parameter decoder: a bit-buffer entropy stage and a four-chain prediction stage.
mod fast {
    use super::*;

    /// Consecutive small symbols after which the narrower ladder applies.
    const RUN_SWITCH: i32 = 3;
    /// Code width as a function of the bit length (0..=16, padded to 32 entries) of the previous
    /// magnitude, for the default ladders, capped at 15 bits.
    const fn width_table(threshold: i32, offset: i32) -> [u8; 32] {
        let mut t = [0u8; 32];
        let mut bits = 0;
        while bits < 32 {
            let mut extra = bits - threshold;
            if extra < -1 {
                extra = -1;
            }
            let width = offset + extra + 1;
            t[bits as usize] = if width > 15 { 15 } else { width as u8 };
            bits += 1;
        }
        t
    }
    pub(super) const RUN_WIDTHS: [u8; 32] = width_table(3, 2);
    pub(super) const BASE_WIDTHS: [u8; 32] = width_table(7, 4);

    /// Rows per band; the residual scratch holds one band of 32-bit codes.
    const BAND_ROWS: usize = 16;
    /// Residual bands in flight between the two stages of the pipeline.
    #[cfg(all(feature = "rayon", not(target_arch = "wasm32")))]
    const PIPELINE_BANDS: usize = 4;

    /// Adaptation state of one column parity.
    #[derive(Clone, Copy, Default)]
    struct Chan {
        smooth: i32,
        run: i32,
        wbits: u32,
    }

    /// The 64 bits at byte offset `at`, zero padded past the end.
    #[inline(always)]
    fn load(d: &[u8], at: usize) -> u64 {
        match d.get(at..at + 8) {
            Some(s) => u64::from_be_bytes(s.try_into().unwrap()),
            None => load_tail(d, at),
        }
    }

    #[cold]
    #[inline(never)]
    fn load_tail(d: &[u8], at: usize) -> u64 {
        let mut b = [0u8; 8];
        if at < d.len() {
            let n = (d.len() - at).min(8);
            b[..n].copy_from_slice(&d[at..at + n]);
        }
        u64::from_be_bytes(b)
    }

    /// Parse one sample and return its residual (smoothing included). `EDGE` marks columns 0 and 1.
    #[inline(always)]
    fn parse<const EDGE: bool>(d: &[u8], pos: &mut u64, c: &mut Chan) -> i32 {
        let wb = if EDGE { 4 } else { c.wbits };
        let w = load(d, (*pos >> 3) as usize) << (*pos & 7);
        let neg = ((w as i64) >> 63) as i32;
        let low = ((w >> 61) & 3) as i32;
        let win = w << 3;
        let z = win.leading_zeros();
        let (glc, used) = if z < 12 {
            let rem = ((win << (z + 1)) >> (64 - wb)) as u32;
            (rem | (z << wb), 4 + z + wb)
        } else {
            let t = win << 12;
            let skip = 15 - wb;
            let high = ((t >> 1) >> (63 - skip)) as u32;
            let rem = (((t << skip) << 1) >> (64 - wb)) as u32;
            (rem | (high << wb), 31)
        };
        *pos += u64::from(used);
        let code = (glc as i32) ^ neg;
        let v = if EDGE { 0 } else { c.smooth };
        let r = low.wrapping_add(v.wrapping_add(code).wrapping_mul(4));
        c.smooth = v.wrapping_add((r >> 2).wrapping_mul(3)) >> 5;
        let small = glc <= 16;
        c.run = if EDGE {
            i32::from(small)
        } else if small {
            c.run + 1
        } else {
            0
        };
        let hb = 32 - (glc & 0xFFFF).leading_zeros() as i32;
        c.wbits =
            u32::from(if c.run >= RUN_SWITCH { RUN_WIDTHS[hb as usize & 31] } else { BASE_WIDTHS[hb as usize & 31] });
        r
    }

    /// Entropy decoder state: bit position and the two channels.
    struct Entropy<'a> {
        body: &'a [u8],
        pos: u64,
        limit: u64,
        ch: [Chan; 2],
    }

    impl Entropy<'_> {
        /// Fill `codes` (`rows` rows of `w`) with residuals. On failure returns the index of the first row
        /// (relative to this band) that ran past the data.
        fn band(&mut self, codes: &mut [i32], w: usize, rows: usize) -> std::result::Result<(), usize> {
            let [mut c0, mut c1] = self.ch;
            let mut pos = self.pos;
            let d = self.body;
            for (i, row) in codes.chunks_exact_mut(w).take(rows).enumerate() {
                row[0] = parse::<true>(d, &mut pos, &mut c0);
                if w > 1 {
                    row[1] = parse::<true>(d, &mut pos, &mut c1);
                }
                let mut pairs = row[2.min(w)..].chunks_exact_mut(2);
                for pair in &mut pairs {
                    pair[0] = parse::<false>(d, &mut pos, &mut c0);
                    pair[1] = parse::<false>(d, &mut pos, &mut c1);
                }
                if let [last] = pairs.into_remainder() {
                    *last = parse::<false>(d, &mut pos, &mut c0);
                }
                if pos > self.limit {
                    return Err(i);
                }
            }
            self.pos = pos;
            self.ch = [c0, c1];
            Ok(())
        }
    }

    /// `x` if `cond` else `y`, without a branch (the outcome depends on the image and mispredicts).
    #[inline(always)]
    fn sel(cond: bool, x: i32, y: i32) -> i32 {
        let m = -i32::from(cond);
        (x & m) | (y & !m)
    }

    /// Prediction from west `a`, north `b` and north-west `c` neighbours.
    #[inline(always)]
    pub(super) fn predict(a: i32, b: i32, c: i32) -> i32 {
        let lo = a.min(b);
        let hi = a.max(b);
        let sum = a.wrapping_add(b);
        let flat = (b.wrapping_sub(c).wrapping_abs() <= 32) & (a.wrapping_sub(c).wrapping_abs() <= 32);
        let inside = sel(flat, sum >> 1, sum.wrapping_sub(c));
        let outside = sel(c >= hi, lo, hi);
        sel((c > lo) & (c < hi), inside, outside)
    }

    /// Reconstruct two consecutive rows. The four chains (two rows, two parities) are independent.
    fn rows_pair(codes: [&[i32]; 2], north: [&[u16]; 2], out: [&mut [u16]; 2]) {
        let [cd0, cd1] = codes;
        let [n0, n1] = north;
        let [o0, o1] = out;
        let w = o0.len();
        let (cd0, cd1, n0, n1, o1) = (&cd0[..w], &cd1[..w], &n0[..w], &n1[..w], &mut o1[..w]);
        let mut west = [0i32; 4];
        let mut nw = [0i32; 4];
        for i in 0..w / 2 {
            let c = 2 * i;
            let north = [i32::from(n0[c]), i32::from(n0[c + 1]), i32::from(n1[c]), i32::from(n1[c + 1])];
            let res = [cd0[c], cd0[c + 1], cd1[c], cd1[c + 1]];
            for k in 0..4 {
                west[k] = res[k].wrapping_add(predict(west[k], north[k], nw[k]));
            }
            o0[c] = west[0] as u16;
            o0[c + 1] = west[1] as u16;
            o1[c] = west[2] as u16;
            o1[c + 1] = west[3] as u16;
            nw = north;
        }
        if w % 2 == 1 {
            let c = w - 1;
            o0[c] = res_one(cd0[c], west[0], i32::from(n0[c]), nw[0]);
            o1[c] = res_one(cd1[c], west[2], i32::from(n1[c]), nw[2]);
        }
    }

    #[inline(always)]
    fn res_one(code: i32, west: i32, north: i32, nw: i32) -> u16 {
        code.wrapping_add(predict(west, north, nw)) as u16
    }

    /// Reconstruct a single row (the last row of a frame with an odd height).
    fn row_single(codes: &[i32], north: &[u16], out: &mut [u16]) {
        let w = out.len();
        let (codes, north) = (&codes[..w], &north[..w]);
        let mut west = [0i32; 2];
        let mut nw = [0i32; 2];
        for i in 0..w / 2 {
            let c = 2 * i;
            let n = [i32::from(north[c]), i32::from(north[c + 1])];
            for k in 0..2 {
                west[k] = codes[c + k].wrapping_add(predict(west[k], n[k], nw[k]));
            }
            out[c] = west[0] as u16;
            out[c + 1] = west[1] as u16;
            nw = n;
        }
        if w % 2 == 1 {
            let c = w - 1;
            out[c] = res_one(codes[c], west[0], i32::from(north[c]), nw[0]);
        }
    }

    /// Turn the residuals of `rows` rows starting at (even) row `r0` into samples.
    fn predict_band(out: &mut [u16], zero: &[u16], w: usize, r0: usize, rows: usize, codes: &[i32]) {
        let mut r = 0;
        while r < rows {
            let row = r0 + r;
            let (before, rest) = out.split_at_mut(row * w);
            let (n0, n1) = if row >= 2 {
                let b = &before[(row - 2) * w..];
                (&b[..w], &b[w..2 * w])
            } else {
                (zero, zero)
            };
            if r + 1 < rows {
                let (o0, o1) = rest[..2 * w].split_at_mut(w);
                rows_pair([&codes[r * w..(r + 1) * w], &codes[(r + 1) * w..(r + 2) * w]], [n0, n1], [o0, o1]);
            } else {
                row_single(&codes[r * w..(r + 1) * w], n0, &mut rest[..w]);
            }
            r += 2;
        }
    }

    pub(super) fn decode(body: &[u8], out: &mut [u16], w: usize, h: usize, parallel: bool) -> Result<()> {
        let mut ent = Entropy { body, pos: 0, limit: bit_limit(body), ch: [Chan::default(); 2] };
        #[cfg(all(feature = "rayon", not(target_arch = "wasm32")))]
        if parallel && h > 2 * BAND_ROWS {
            return pipelined(&mut ent, out, w, h);
        }
        let _ = parallel;
        let zero = vec![0u16; w];
        let mut codes = vec![0i32; BAND_ROWS * w];
        let mut r0 = 0;
        while r0 < h {
            let rows = BAND_ROWS.min(h - r0);
            ent.band(&mut codes, w, rows).map_err(|i| exhausted(r0 + i, h))?;
            predict_band(out, &zero, w, r0, rows, &codes);
            r0 += rows;
        }
        Ok(())
    }

    /// Entropy stage on the calling thread, prediction stage on a second one.
    #[cfg(all(feature = "rayon", not(target_arch = "wasm32")))]
    fn pipelined(ent: &mut Entropy<'_>, out: &mut [u16], w: usize, h: usize) -> Result<()> {
        use std::sync::mpsc;
        let (full_tx, full_rx) = mpsc::sync_channel::<(Vec<i32>, usize, usize)>(PIPELINE_BANDS);
        let (free_tx, free_rx) = mpsc::channel::<Vec<i32>>();
        for _ in 0..PIPELINE_BANDS {
            let _ = free_tx.send(vec![0i32; BAND_ROWS * w]);
        }
        std::thread::scope(|s| {
            s.spawn(move || {
                let zero = vec![0u16; w];
                for (codes, r0, rows) in full_rx {
                    predict_band(out, &zero, w, r0, rows, &codes);
                    let _ = free_tx.send(codes);
                }
            });
            let mut r0 = 0;
            let mut result = Ok(());
            while r0 < h {
                let rows = BAND_ROWS.min(h - r0);
                let Ok(mut codes) = free_rx.recv() else { break };
                if let Err(i) = ent.band(&mut codes, w, rows) {
                    result = Err(exhausted(r0 + i, h));
                    break;
                }
                if full_tx.send((codes, r0, rows)).is_err() {
                    break;
                }
                r0 += rows;
            }
            // Closing the channel lets the prediction thread finish before the scope joins it.
            drop(full_tx);
            result
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
    }

    /// A stream with the standard header followed by `n` pseudo-random bytes of one of five styles:
    /// uniform, zero-heavy (escapes), small codes (runs), 0xff/0x00 bursts, high bit set.
    fn stream(seed: u64, n: usize, style: u32) -> Vec<u8> {
        let mut r = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let mut v = vec![0, 0, 0, 0, 1, 0, 0];
        for _ in 0..n {
            let x = r.next();
            v.push(match style {
                0 => (x >> 24) as u8,
                1 => {
                    if x & 7 == 0 {
                        (x >> 32) as u8
                    } else {
                        0
                    }
                }
                2 => ((x >> 24) as u8) & ((x >> 40) as u8) & ((x >> 48) as u8),
                3 => {
                    if x & 64 == 0 {
                        0xFF
                    } else {
                        0
                    }
                }
                _ => ((x >> 24) as u8) | 0x80,
            });
        }
        v
    }

    fn fnv(v: &[u16]) -> u64 {
        v.iter()
            .flat_map(|x| x.to_le_bytes())
            .fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3))
    }

    fn run(
        data: &[u8],
        p: &OlympusParams,
        w: usize,
        h: usize,
        parallel: bool,
    ) -> (std::result::Result<(), String>, Vec<u16>) {
        let mut out = vec![0u16; w * h];
        let r = decode_with(data, p, &mut out, w, h, parallel).map_err(|e| e.to_string());
        (r, out)
    }

    #[test]
    fn pipelined_equals_sequential() {
        let p = OlympusParams::default();
        for style in 0..5 {
            for &(w, h, n) in &[(97usize, 101usize, 60_000usize), (64, 70, 2_000), (33, 65, 9_000)] {
                let data = stream(u64::from(style) + 1, n, style);
                let (ra, a) = run(&data, &p, w, h, false);
                let (rb, b) = run(&data, &p, w, h, true);
                assert_eq!(ra, rb);
                if ra.is_ok() {
                    assert_eq!(a, b);
                }
            }
        }
    }

    /// The default-parameter fast path against the general decoder (only on streams long enough that neither runs
    /// into the end of the data: the general decoder rejects slightly earlier near the end).
    #[test]
    fn fast_path_matches_general_decoder() {
        let p = OlympusParams::default();
        for style in 0..5 {
            for &(w, h) in &[(1usize, 9usize), (2, 5), (3, 7), (31, 40), (64, 33), (97, 36)] {
                let data = stream(100 + u64::from(style), w * h * 5 + 64, style);
                let (fast, a) = run(&data, &p, w, h, false);
                let mut b = vec![0u16; w * h];
                let slow = general::decode(&data[7..], &p, &mut b, w, h).map_err(|e| e.to_string());
                assert_eq!(fast, slow, "{w}x{h} style {style}");
                assert_eq!(a, b, "{w}x{h} style {style}");
            }
        }
    }

    /// Prediction rule of the format against an independent nested-comparison statement of it.
    #[test]
    fn predictor_matches_description() {
        fn describe(w: i32, n: i32, nw: i32, g: i32) -> i32 {
            if (n < nw && nw < w) || (w < nw && nw < n) {
                if (n - nw).abs() <= g && (w - nw).abs() <= g { (n + w).div_euclid(2) } else { n + w - nw }
            } else if nw >= w && nw >= n {
                w.min(n)
            } else {
                w.max(n)
            }
        }
        let vals: Vec<i32> = (-70..=70).chain([255, 4095, 16383, 65535, 70000]).collect();
        for &a in &vals {
            for &b in &vals {
                for &c in &vals {
                    assert_eq!(fast::predict(a, b, c), describe(a, b, c, 32), "{a} {b} {c}");
                    assert_eq!(general::predict(a, b, c, 1 << 16), describe(a, b, c, 1 << 16));
                }
            }
        }
    }

    #[test]
    fn header_and_buffer_checks() {
        let p = OlympusParams::default();
        let ok = [0u8, 0, 0, 0, 1, 0, 0];
        assert_eq!(stream_start(&ok, &p).unwrap(), 7);
        for len in 0..7 {
            assert!(stream_start(&ok[..len], &p).is_err());
        }
        for i in 0..7 {
            let mut bad = ok;
            bad[i] ^= 0x10;
            assert!(stream_start(&bad, &p).unwrap_err().to_string().contains("invalid compressed stream header"));
        }
        let mut out = [0u16; 4];
        assert!(decode(&ok, &p, &mut out, 3, 2).unwrap_err().to_string().contains("output buffer too small"));
        assert!(decode(&ok, &p, &mut out, 0, 5).is_ok());
        let bad = OlympusParams { extra_bits: 5, ..p };
        assert_eq!(decode(&ok, &bad, &mut out, 2, 2).unwrap_err().kind(), "UnsupportedCompression");
        let wide = OlympusParams { max_wbits: 40, ..p };
        assert_eq!(decode(&ok, &wide, &mut out, 2, 2).unwrap_err().kind(), "UnsupportedCompression");
    }

    #[test]
    fn running_out_of_data_reports_the_row() {
        let p = OlympusParams::default();
        let data = stream(7, 400, 0);
        let (r, _) = run(&data, &p, 64, 64, false);
        let msg = r.unwrap_err();
        assert!(msg.contains("compressed data ends at row"), "{msg}");
        for parallel in [false, true] {
            assert_eq!(run(&data, &p, 64, 64, parallel).0.unwrap_err(), msg);
        }
        // An all-zero tail is accepted for exactly 8 bytes past the end: two escapes per row at 62 bits.
        let zeros = [0u8, 0, 0, 0, 1, 0, 0];
        assert!(run(&zeros, &p, 2, 1, false).0.is_ok());
        assert!(run(&zeros, &p, 2, 2, false).0.is_err());
    }

    /// Outputs of fixed pseudo-random streams, pinned from a run that matched the previous implementation bit
    /// for bit (results and error messages) on these and thousands of other streams and parameter sets.
    #[test]
    fn golden_outputs() {
        let d = OlympusParams::default();
        let sets: [OlympusParams; 6] = [
            d,
            OlympusParams { grad_shift: 4, ..d },
            OlympusParams { low_bits: 1, smooth_shift: 3, ..d },
            OlympusParams { extra_bits: 2, edge_pred: 100, ..d },
            OlympusParams { small_shift: 6, run_switch: 2, max_high: 8, ..d },
            OlympusParams { low_bits: 0, base_offset: 3, max_wbits: 12, header_len: 5, ..d },
        ];
        let mut got = Vec::new();
        for (si, p) in sets.iter().enumerate() {
            for style in 0..5u32 {
                for cut in [3usize, 1] {
                    let (w, h) = (37 + si * 5, 41 + style as usize);
                    let n = if cut == 3 { w * h * 3 } else { w * h * 3 / 8 };
                    let data = stream(1000 + si as u64 * 10 + u64::from(style), n, style);
                    let (r, out) = run(&data, p, w, h, false);
                    got.push(format!(
                        "{si} {style} {cut} {}",
                        r.map(|_| format!("{:016x}", fnv(&out))).unwrap_or_else(|e| e.replace("corrupt file: ", ""))
                    ));
                }
            }
        }
        const EXPECTED: [&str; 60] = [
            "0 0 3 fb3537f4e6ea457d",
            "0 0 1 ORF: compressed data ends at row 13/41",
            "0 1 3 ORF: compressed data ends at row 38/42",
            "0 1 1 ORF: compressed data ends at row 4/42",
            "0 2 3 291a80626626f216",
            "0 2 1 ORF: compressed data ends at row 6/43",
            "0 3 3 8f4e013b742c12a2",
            "0 3 1 ORF: compressed data ends at row 8/44",
            "0 4 3 0385ab0ececef38b",
            "0 4 1 ORF: compressed data ends at row 17/45",
            "1 0 3 b4121c89633e976c",
            "1 0 1 ORF: compressed data ends at row 13/41",
            "1 1 3 ORF: compressed data ends at row 38/42",
            "1 1 1 ORF: compressed data ends at row 4/42",
            "1 2 3 0f4856254d166aaf",
            "1 2 1 ORF: compressed data ends at row 7/43",
            "1 3 3 d92270083b0834a4",
            "1 3 1 ORF: compressed data ends at row 8/44",
            "1 4 3 d115dd138aedae3c",
            "1 4 1 ORF: compressed data ends at row 16/45",
            "2 0 3 2df606b466382cee",
            "2 0 1 ORF: compressed data ends at row 15/41",
            "2 1 3 ORF: compressed data ends at row 41/42",
            "2 1 1 ORF: compressed data ends at row 5/42",
            "2 2 3 16ba9fe015516fef",
            "2 2 1 ORF: compressed data ends at row 7/43",
            "2 3 3 780acf998ef8facb",
            "2 3 1 ORF: compressed data ends at row 10/44",
            "2 4 3 bf9eaa80949b4a7c",
            "2 4 1 ORF: compressed data ends at row 17/45",
            "3 0 3 ebe5c85c015f0166",
            "3 0 1 ORF: compressed data ends at row 11/41",
            "3 1 3 ORF: compressed data ends at row 35/42",
            "3 1 1 ORF: compressed data ends at row 4/42",
            "3 2 3 2ebc04a69690e315",
            "3 2 1 ORF: compressed data ends at row 6/43",
            "3 3 3 d110e3fd2a8065e6",
            "3 3 1 ORF: compressed data ends at row 7/44",
            "3 4 3 c270beaad62a2659",
            "3 4 1 ORF: compressed data ends at row 12/45",
            "4 0 3 a876758dd1c0dc6f",
            "4 0 1 ORF: compressed data ends at row 13/41",
            "4 1 3 3afd8ed76f4303c0",
            "4 1 1 ORF: compressed data ends at row 5/42",
            "4 2 3 aa1209cae9bd313c",
            "4 2 1 ORF: compressed data ends at row 6/43",
            "4 3 3 e75f014a4103e59a",
            "4 3 1 ORF: compressed data ends at row 8/44",
            "4 4 3 f830d220ae3961f0",
            "4 4 1 ORF: compressed data ends at row 15/45",
            "5 0 3 d6da125667cb74cc",
            "5 0 1 ORF: compressed data ends at row 20/41",
            "5 1 3 cbe7e649f39b6105",
            "5 1 1 ORF: compressed data ends at row 5/42",
            "5 2 3 9332e0e3eab7db5c",
            "5 2 1 ORF: compressed data ends at row 8/43",
            "5 3 3 0c4321570d851219",
            "5 3 1 ORF: compressed data ends at row 12/44",
            "5 4 3 f2d0e2a7998970c8",
            "5 4 1 ORF: compressed data ends at row 23/45",
        ];
        assert_eq!(got, EXPECTED);
    }
}
