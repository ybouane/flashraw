//! Line decoders of the subband entropy streams, and the [`BandReader`] that couples one with the dequantiser.
//!
//! Three coders exist: a predictive one (lossless planes and the coarsest wavelet band), its near-lossless
//! variant, and the zero-run coder of the wavelet detail bands. Each keeps the context it needs from the line
//! above and delivers one line of `i32` coefficients per call.

use super::bits::CrxBits;
use super::prim::{K_MAX, QP_ESCAPE, RunState, SAMPLE_ESCAPE, adapt, adapt_free, predict, unzigzag};
use super::quant::{MapScale, step};
use flashraw_core::{Error, Result};

const SAMPLE: (u32, u32) = SAMPLE_ESCAPE;

#[inline(always)]
fn sample_code(bits: &mut CrxBits, k: u32) -> u32 {
    bits.code(k, SAMPLE.0, SAMPLE.1)
}

/// `|v|` as the unsigned value the Rice adaptation sees for `2 * v`.
#[inline(always)]
fn twice_abs(v: i32) -> u32 {
    v.wrapping_mul(2).wrapping_abs() as u32
}

// ---------------------------------------------------------------------------------------------------------------
// Predictive coder
// ---------------------------------------------------------------------------------------------------------------

/// Predictive coder. Lines are kept with one pad entry in front: entry `1 + x` is sample `x`.
struct Predictive {
    width: usize,
    k: u32,
    run: RunState,
    above: Vec<i32>,
    line: Vec<i32>,
    y: usize,
}

impl Predictive {
    fn new(width: usize) -> Self {
        Predictive { width, k: 0, run: RunState::default(), above: vec![0; width + 2], line: vec![0; width + 2], y: 0 }
    }

    fn decode(&mut self, bits: &mut CrxBits) -> Result<()> {
        if self.y == 0 {
            self.top_line(bits)?;
        } else {
            self.next_line(bits)?;
        }
        std::mem::swap(&mut self.above, &mut self.line);
        self.y += 1;
        Ok(())
    }

    fn samples(&self) -> &[i32] {
        &self.above[1..=self.width]
    }

    fn top_line(&mut self, bits: &mut CrxBits) -> Result<()> {
        let w = self.width;
        let out = &mut self.line[..w + 1];
        out[0] = 0;
        let mut k = self.k;
        let mut prev = 0i32;
        let mut x = 0usize;
        while x + 1 < w {
            let base = if prev != 0 {
                prev
            } else {
                if bits.bit() {
                    let n = self.run.read(bits, w - x)?;
                    out[1 + x..1 + x + n].fill(0);
                    x += n;
                    if x == w {
                        self.k = k;
                        return Ok(());
                    }
                }
                0
            };
            let c = sample_code(bits, k);
            prev = base.wrapping_add(unzigzag(c));
            out[1 + x] = prev;
            k = adapt(k, c, K_MAX);
            x += 1;
        }
        if x + 1 == w {
            let c = sample_code(bits, k);
            out[1 + x] = prev.wrapping_add(unzigzag(c));
            k = adapt(k, c, K_MAX);
        }
        self.k = k;
        Ok(())
    }

    fn next_line(&mut self, bits: &mut CrxBits) -> Result<()> {
        let w = self.width;
        let above = &self.above[..w + 2];
        let out = &mut self.line[..w + 1];
        out[0] = above[1];
        let mut k = self.k;
        let mut left = above[1];
        let mut x = 0usize;
        while x + 1 < w {
            let top = above[x + 1];
            let top_right = above[x + 2];
            if left != top || left != top_right {
                let c = sample_code(bits, k);
                left = predict(left, top, above[x]).wrapping_add(unzigzag(c));
                out[1 + x] = left;
                k = adapt(k, c.wrapping_add(twice_abs(top_right.wrapping_sub(top))) >> 1, K_MAX);
                x += 1;
            } else {
                if bits.bit() {
                    let n = self.run.read(bits, w - x)?;
                    out[1 + x..1 + x + n].fill(left);
                    x += n;
                    if x == w {
                        self.k = k;
                        return Ok(());
                    }
                }
                let c = sample_code(bits, k);
                left = above[x + 1].wrapping_add(unzigzag(c));
                out[1 + x] = left;
                k = if x + 1 < w {
                    adapt(k, c.wrapping_add(twice_abs(above[x + 2].wrapping_sub(above[x + 1]))) >> 1, K_MAX)
                } else {
                    adapt(k, c, K_MAX)
                };
                x += 1;
            }
        }
        if x + 1 == w {
            let c = sample_code(bits, k);
            out[1 + x] = predict(left, above[x + 1], above[x]).wrapping_add(unzigzag(c));
            k = adapt(k, c, K_MAX);
        }
        self.k = k;
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Near-lossless predictive coder
// ---------------------------------------------------------------------------------------------------------------

/// Predictive coder whose residuals are quantised with step `2 * m` (`m = 1 << (rb - 1)`).
struct NearLossless {
    inner: Predictive,
    m: i32,
    rb: u32,
}

impl NearLossless {
    fn new(width: usize, rounded_bits: u32) -> Self {
        NearLossless { inner: Predictive::new(width), m: 1 << (rounded_bits - 1), rb: rounded_bits }
    }

    #[inline(always)]
    fn scaled(&self, v: i32) -> i32 {
        (2i32.wrapping_mul(self.m)).wrapping_mul(v).wrapping_add(v >> 31)
    }

    fn decode(&mut self, bits: &mut CrxBits) -> Result<()> {
        if self.inner.y == 0 {
            self.top_line(bits)?;
        } else {
            self.next_line(bits)?;
        }
        std::mem::swap(&mut self.inner.above, &mut self.inner.line);
        self.inner.y += 1;
        Ok(())
    }

    fn top_line(&mut self, bits: &mut CrxBits) -> Result<()> {
        let m = self.m;
        let w = self.inner.width;
        let mut k = self.inner.k;
        let mut prev = 0i32;
        let mut x = 0usize;
        self.inner.line[0] = 0;
        while x + 1 < w {
            let base = if prev.wrapping_abs() > m {
                prev
            } else {
                if bits.bit() {
                    let n = self.inner.run.read(bits, w - x)?;
                    self.inner.line[1 + x..1 + x + n].fill(prev);
                    x += n;
                    if x == w {
                        self.inner.k = k;
                        return Ok(());
                    }
                }
                0
            };
            let c = sample_code(bits, k);
            prev = base.wrapping_add(self.scaled(unzigzag(c)));
            self.inner.line[1 + x] = prev;
            k = adapt(k, c, K_MAX);
            x += 1;
        }
        if x + 1 == w {
            let c = sample_code(bits, k);
            self.inner.line[1 + x] = self.scaled(unzigzag(c));
            k = adapt(k, c, K_MAX);
        }
        self.inner.k = k;
        Ok(())
    }

    fn next_line(&mut self, bits: &mut CrxBits) -> Result<()> {
        let (m, rb) = (self.m, self.rb);
        let w = self.inner.width;
        self.inner.above[0] = self.inner.above[1];
        self.inner.line[0] = self.inner.above[1];
        let mut k = self.inner.k;
        let mut reached = false;
        let mut x = 0usize;

        // Decode one sample at `x` with the given base choice; returns the adapted Rice parameter.
        macro_rules! sample {
            ($pred:expr, $nbr:expr) => {{
                let above = &self.inner.above;
                let base = if $pred { predict(self.inner.line[x], above[x + 1], above[x]) } else { above[x + 1] };
                let c = sample_code(bits, k);
                self.inner.line[x + 1] = base.wrapping_add(self.scaled(unzigzag(c)));
                if $nbr {
                    let (a, n) = (above[x + 2], above[x + 1]);
                    let d = if a > n {
                        a.wrapping_sub(n).wrapping_add(m - 1) >> rb
                    } else {
                        n.wrapping_sub(a).wrapping_add(m) >> rb
                    };
                    let d = if a > n { d } else { d.wrapping_neg() };
                    k = adapt(k, c.wrapping_add(twice_abs(d)) >> 1, K_MAX);
                } else {
                    k = adapt(k, c, K_MAX);
                }
            }};
        }

        while x + 1 < w {
            let (t, tl, tr, left) =
                (self.inner.above[x + 1], self.inner.above[x], self.inner.above[x + 2], self.inner.line[x]);
            if tr.wrapping_sub(t).wrapping_abs() > m {
                sample!(true, true);
                reached = true;
                x += 1;
            } else if reached || tl.wrapping_sub(left).wrapping_abs() > m {
                sample!(true, true);
                reached = false;
                x += 1;
            } else {
                if bits.bit() {
                    let n = self.inner.run.read(bits, w - x)?;
                    self.inner.line[x + 1..x + 1 + n].fill(left);
                    x += n;
                }
                let rem = w - x;
                if rem > 1 {
                    sample!(false, true);
                    x += 1;
                    reached = self.inner.above[x + 1].wrapping_sub(self.inner.above[x]).wrapping_abs() > m;
                } else if rem == 1 {
                    sample!(false, false);
                    x += 1;
                }
            }
        }
        if x + 1 == w {
            sample!(true, false);
        }
        self.inner.k = k;
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Detail (zero-run) coder
// ---------------------------------------------------------------------------------------------------------------

/// Coder of wavelet detail bands: zero runs, and a per-column memory of the Rice parameter used above.
struct Detail {
    width: usize,
    k: u32,
    run: RunState,
    memory: Vec<i32>,
    above: Vec<i32>,
    line: Vec<i32>,
    y: usize,
}

/// Rice parameter for the next sample: follow the one used above at the next column when it is larger.
#[inline(always)]
fn follow(k: u32, above: i32) -> u32 {
    let k = k as i32;
    if above - k <= 1 { k.min(K_MAX as i32) as u32 } else { (k + 1) as u32 }
}

impl Detail {
    fn new(width: usize) -> Self {
        Detail {
            width,
            k: 0,
            run: RunState::default(),
            memory: vec![0; width],
            above: vec![0; width],
            line: vec![0; width],
            y: 0,
        }
    }

    fn decode(&mut self, bits: &mut CrxBits) -> Result<()> {
        if self.y == 0 {
            self.top_line(bits)?;
        } else {
            self.next_line(bits)?;
        }
        std::mem::swap(&mut self.above, &mut self.line);
        self.y += 1;
        Ok(())
    }

    fn top_line(&mut self, bits: &mut CrxBits) -> Result<()> {
        let w = self.width;
        let (out, mem) = (&mut self.line[..w], &mut self.memory[..w]);
        let mut k = self.k;
        let mut prev = 0i32;
        let mut x = 0usize;
        while x + 1 < w {
            let v;
            if prev != 0 {
                let c = sample_code(bits, k);
                v = unzigzag(c);
                k = adapt(k, c, K_MAX);
            } else {
                if bits.bit() {
                    let n = self.run.read(bits, w - x)?;
                    out[x..x + n].fill(0);
                    mem[x..x + n].fill(0);
                    x += n;
                    if x == w {
                        self.k = k;
                        return Ok(());
                    }
                }
                let c = sample_code(bits, k);
                v = unzigzag(c.wrapping_add(1));
                k = adapt(k, c, K_MAX);
            }
            out[x] = v;
            prev = v;
            mem[x] = k as i32;
            x += 1;
        }
        if x + 1 == w {
            let c = sample_code(bits, k);
            out[x] = unzigzag(c);
            k = adapt(k, c, K_MAX);
            mem[x] = k as i32;
        }
        self.k = k;
        Ok(())
    }

    fn next_line(&mut self, bits: &mut CrxBits) -> Result<()> {
        let w = self.width;
        let above = &self.above[..w];
        let out = &mut self.line[..w];
        let mem = &mut self.memory[..w];
        let mut k = self.k;
        let mut left = 0i32;
        let mut i = 0usize;
        while i + 1 < w {
            let v;
            if (above[i + 1] | above[i] | left) != 0 {
                let c = sample_code(bits, k);
                v = unzigzag(c);
                k = follow(adapt_free(k, c), mem[i + 1]);
            } else {
                if bits.bit() {
                    let n = self.run.read(bits, w - i)?;
                    out[i..i + n].fill(0);
                    mem[i..i + n].fill(0);
                    i += n;
                }
                if i + 1 >= w {
                    if i + 1 == w {
                        let c = sample_code(bits, k);
                        out[i] = unzigzag(c.wrapping_add(1));
                        k = adapt(k, c, K_MAX);
                        mem[i] = k as i32;
                    }
                    self.k = k;
                    return Ok(());
                }
                let c = sample_code(bits, k);
                v = unzigzag(c.wrapping_add(1));
                k = follow(adapt_free(k, c), mem[i + 1]);
            }
            out[i] = v;
            left = v;
            mem[i] = k as i32;
            i += 1;
        }
        if i + 1 == w {
            let c = sample_code(bits, k);
            out[i] = unzigzag(c);
            k = adapt(k, c, K_MAX);
            mem[i] = k as i32;
        }
        self.k = k;
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Band reader
// ---------------------------------------------------------------------------------------------------------------

enum Coder {
    Predictive(Predictive),
    Near(NearLossless),
    Detail(Detail),
}

/// How a decoded line becomes a line of coefficients.
pub(super) enum Dequant<'a> {
    /// Lossless data.
    None,
    /// One factor per line; `deltas` set means the parameter is updated from the band's own stream before each line.
    Uniform { q: i32, deltas: Option<u32> },
    /// A factor per position taken from the tile's quantisation map.
    Mapped(MapScale<'a>),
}

/// Which line coder a band uses.
#[derive(Clone, Copy)]
pub(super) enum Kind {
    Predictive,
    /// Predictive with `rounded_bits` in 1..=3.
    Near(u32),
    Detail,
}

/// One subband: its bit stream, line decoder and dequantiser.
pub(super) struct BandReader<'a> {
    bits: CrxBits<'a>,
    coder: Coder,
    dequant: Dequant<'a>,
    height: usize,
    lines: usize,
    /// A band without stream bytes is all zeros.
    empty: bool,
}

impl<'a> BandReader<'a> {
    pub(super) fn new(
        stream: &'a [u8],
        width: usize,
        height: usize,
        kind: Kind,
        dequant: Dequant<'a>,
    ) -> BandReader<'a> {
        let coder = match kind {
            Kind::Predictive => Coder::Predictive(Predictive::new(width)),
            Kind::Near(rb) => Coder::Near(NearLossless::new(width, rb)),
            Kind::Detail => Coder::Detail(Detail::new(width)),
        };
        BandReader { bits: CrxBits::new(stream), coder, dequant, height, lines: 0, empty: stream.is_empty() }
    }

    /// Decode the next line (before dequantisation) into the coder's buffers.
    fn advance(&mut self) -> Result<()> {
        if self.lines >= self.height {
            return Err(Error::corrupt("CRX: subband has fewer lines than required"));
        }
        if let Dequant::Uniform { q, deltas: Some(kq) } = &mut self.dequant {
            let c = self.bits.code(*kq, QP_ESCAPE.0, QP_ESCAPE.1);
            *q = q.wrapping_add(unzigzag(c));
            *kq = adapt_free(*kq, c);
            if *kq > 7 {
                return Err(Error::corrupt("CRX: invalid quantisation parameter code"));
            }
        }
        match &mut self.coder {
            Coder::Predictive(c) => c.decode(&mut self.bits)?,
            Coder::Near(c) => c.decode(&mut self.bits)?,
            Coder::Detail(c) => c.decode(&mut self.bits)?,
        }
        if self.bits.exhausted() {
            return Err(Error::corrupt("CRX: truncated subband bit stream"));
        }
        self.lines += 1;
        Ok(())
    }

    fn line(&self) -> &[i32] {
        match &self.coder {
            Coder::Predictive(c) => c.samples(),
            Coder::Near(c) => c.inner.samples(),
            Coder::Detail(c) => &c.above[..c.width],
        }
    }

    /// The next line of an undequantised band (lossless planes).
    pub(super) fn next_unscaled(&mut self) -> Result<&[i32]> {
        self.advance()?;
        Ok(self.line())
    }

    /// Decode the next line, dequantise it and store it in `dst` (the band's width).
    pub(super) fn next_scaled(&mut self, dst: &mut [i32]) -> Result<()> {
        if self.empty {
            dst.fill(0);
            return Ok(());
        }
        let row = self.lines;
        self.advance()?;
        let src = match &self.coder {
            Coder::Predictive(c) => c.samples(),
            Coder::Near(c) => c.inner.samples(),
            Coder::Detail(c) => &c.above[..c.width],
        };
        match &self.dequant {
            Dequant::None => dst.copy_from_slice(src),
            Dequant::Uniform { q, .. } => {
                let f = step(*q) as i32;
                for (o, &v) in dst.iter_mut().zip(src) {
                    *o = v.wrapping_mul(f);
                }
            }
            Dequant::Mapped(m) => m.scale_line(row, src, dst),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// MSB-first bit writer.
    #[derive(Default)]
    struct BitWriter {
        bytes: Vec<u8>,
        used: u32,
    }

    impl BitWriter {
        fn bit(&mut self, b: bool) {
            if self.used % 8 == 0 {
                self.bytes.push(0);
            }
            if b {
                *self.bytes.last_mut().unwrap() |= 0x80 >> (self.used % 8);
            }
            self.used += 1;
        }
        fn code(&mut self, v: u32, k: u32) {
            if v >> k >= 41 {
                // escape: a prefix of 41 zeros, then the value in 21 raw bits
                for _ in 0..41 {
                    self.bit(false);
                }
                self.bit(true);
                for i in (0..21).rev() {
                    self.bit((v >> i) & 1 == 1);
                }
                return;
            }
            for _ in 0..v >> k {
                self.bit(false);
            }
            self.bit(true);
            for i in (0..k).rev() {
                self.bit((v >> i) & 1 == 1);
            }
        }
        fn finish(mut self) -> Vec<u8> {
            self.bytes.extend_from_slice(&[0; 16]);
            self.bytes
        }
    }

    fn zigzag(v: i32) -> u32 {
        ((v << 1) ^ (v >> 31)) as u32
    }

    /// Encoder of lossless predictive planes that never uses run codes (the run flag is always 0).
    fn encode_lossless(img: &[Vec<i32>]) -> Vec<u8> {
        let w = img[0].len();
        let mut bw = BitWriter::default();
        let mut k = 0u32;
        let mut above = vec![0i32; w + 2];
        for (y, row) in img.iter().enumerate() {
            let mut line = vec![0i32; w + 2];
            let mut left = if y == 0 { 0 } else { above[1] };
            line[0] = left;
            for (x, &v) in row.iter().enumerate() {
                let last = x + 1 == w;
                let (t, tl, tr) = (above[x + 1], above[x], above[x + 2]);
                // (prediction, value the next Rice parameter is derived from)
                let (pred, gradient) = if y == 0 {
                    if left == 0 && !last {
                        bw.bit(false);
                    }
                    (left, 0)
                } else if last {
                    (predict(left, t, tl), 0)
                } else if left != t || left != tr {
                    (predict(left, t, tl), twice_abs(tr.wrapping_sub(t)))
                } else {
                    bw.bit(false);
                    (t, twice_abs(above[x + 2].wrapping_sub(t)))
                };
                let c = zigzag(v.wrapping_sub(pred));
                bw.code(c, k);
                // the first line and the last sample of a line adapt from the code alone
                k = if y == 0 || last { adapt(k, c, K_MAX) } else { adapt(k, c.wrapping_add(gradient) >> 1, K_MAX) };
                left = v;
                line[1 + x] = v;
            }
            above = line;
        }
        bw.finish()
    }

    #[test]
    fn lossless_planes_round_trip() {
        let mut seed = 0x1234_5678u32;
        let mut rnd = move |m: u32| {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            (seed >> 8) % m
        };
        for &(w, h) in &[(1usize, 4usize), (2, 3), (3, 3), (22, 9), (31, 17), (64, 8)] {
            // smooth gradients with flat patches and some noise, so that busy and flat contexts both occur
            let img: Vec<Vec<i32>> = (0..h)
                .map(|y| {
                    (0..w)
                        .map(|x| match (x / 5 + y / 3) % 3 {
                            0 => 100,
                            1 => 100 + (x + y) as i32 + rnd(4) as i32,
                            _ => rnd(2000) as i32 - 1000,
                        })
                        .collect()
                })
                .collect();
            let stream = encode_lossless(&img);
            let mut band = BandReader::new(&stream, w, h, Kind::Predictive, Dequant::None);
            for (y, row) in img.iter().enumerate() {
                assert_eq!(band.next_unscaled().unwrap(), &row[..], "{w}x{h} line {y}");
            }
            // one more line than the band has is an error
            assert!(band.next_unscaled().is_err());
        }
    }

    #[test]
    fn uniform_dequantisation_scales_every_line() {
        let img = vec![vec![1, -2, 3, 0, 5], vec![0, 0, 0, 7, -7]];
        let stream = encode_lossless(&img);
        let mut band = BandReader::new(&stream, 5, 2, Kind::Predictive, Dequant::Uniform { q: 42, deltas: None });
        let mut line = [0i32; 5];
        band.next_scaled(&mut line).unwrap();
        assert_eq!(line, [80, -160, 240, 0, 400]);
        band.next_scaled(&mut line).unwrap();
        assert_eq!(line, [0, 0, 0, 560, -560]);
    }

    #[test]
    fn empty_band_is_all_zeros_for_ever() {
        let mut band = BandReader::new(&[], 4, 1, Kind::Detail, Dequant::Uniform { q: 40, deltas: None });
        let mut line = [9i32; 4];
        for _ in 0..3 {
            band.next_scaled(&mut line).unwrap();
            assert_eq!(line, [0; 4]);
        }
    }
}
