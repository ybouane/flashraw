//! Olympus / OM System ORF lossless compression.
//!
//! A single MSB-first bit stream codes every sample of the frame as an adaptive
//! Golomb-like residual. Even and odd columns form two interleaved channels: each
//! channel keeps its own adaptation state (previous code magnitude, a run counter
//! of small codes and a smoothed residual), and predicts from same-colour
//! neighbours two pixels to the left (`W`), above (`N`) and diagonal (`NW`).
//!
//! Per sample:
//! 1. sign bit, then `low_bits` (usually 2) literal low bits, then `extra_bits`
//!    (usually 0) bits appended below the reconstructed value;
//! 2. a unary-coded "high" part (number of zeros before a one, escape after
//!    `max_high` zeros to a fixed-width field), then `wbits` "low" bits, where
//!    `wbits` adapts to the magnitude of the previous same-channel code;
//! 3. the code is combined with the smoothed residual and added to the
//!    edge-sensitive median prediction.
//!
//! The camera describes the coder with tags 0x640..0x653 of the makernote
//! ImageProcessing directory ([`OlympusParams`]); every body to date uses the same
//! values, which are also the defaults. The stream is inherently sequential.

use flashraw_core::bits::{BitPumpMsb, BitRead};
use flashraw_core::{Error, Result};

/// Coder parameters (makernote ImageProcessing tags 0x640..0x653).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OlympusParams {
    /// 0x640: extra literal low bits per sample (value is shifted left by this).
    pub extra_bits: u32,
    /// 0x641: log2 of the gradient threshold of the predictor.
    pub grad_shift: u32,
    /// 0x642: prediction used outside the image (first rows/columns).
    pub edge_pred: i32,
    /// 0x643: literal low bits coded before the adaptive part (0, 1 or 2).
    pub low_bits: u32,
    /// 0x644: smoothing shift of the residual accumulator (15 disables it).
    pub smooth_shift: u32,
    /// 0x645: codes up to `1 << small_shift` count as "small" for the run counter.
    pub small_shift: u32,
    /// 0x646: run length of small codes that switches to the fine width model.
    pub run_switch: i32,
    /// 0x647/0x648: width model after a run (offset, magnitude threshold).
    pub run_offset: i32,
    pub run_threshold: i32,
    /// 0x649/0x650: default width model (offset, magnitude threshold).
    pub base_offset: i32,
    pub base_threshold: i32,
    /// 0x651: maximum adaptive width.
    pub max_wbits: i32,
    /// 0x652: unary escape length.
    pub max_high: u32,
    /// 0x653: length of the big-endian stream header (value 1) preceding two zero bytes.
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
    /// Build from the 14 tag values 0x640..=0x653 (`None` = default).
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

    fn validate(&self) -> Result<()> {
        let ok = self.extra_bits <= 4
            && self.grad_shift <= 16
            && self.low_bits <= 2
            && self.small_shift <= 31
            && self.max_high <= 24
            && (1..=8).contains(&self.header_len)
            && (0..=15).contains(&self.max_wbits);
        if ok { Ok(()) } else { Err(Error::unsupported_compression("ORF", format!("coder parameters {self:?}"))) }
    }
}

/// Offset of the bit stream after the header, checking the header contents.
pub fn stream_start(data: &[u8], p: &OlympusParams) -> Result<usize> {
    let n = p.header_len as usize;
    let hdr = data.get(..n + 2).ok_or_else(|| Error::corrupt("ORF: compressed data too short"))?;
    let v = hdr[..n].iter().fold(0u64, |a, &b| a << 8 | b as u64);
    if v != 1 || hdr[n] != 0 || hdr[n + 1] != 0 {
        return Err(Error::corrupt("ORF: bad compressed stream header"));
    }
    Ok(n + 2)
}

/// Per-channel (even/odd column) adaptation state.
#[derive(Clone, Copy, Default)]
struct Chan {
    /// Previous code magnitude (`glc`).
    prev_code: u32,
    /// Run counter of small codes.
    run: i32,
    /// Smoothed residual (`vbits >> smooth_shift` of the previous sample).
    smooth: i32,
    /// Previous reconstructed value of this channel (unshifted, untruncated).
    west: i32,
}

/// Decode a full `width x height` frame into `out` (row-major, `width` stride).
/// `data` starts at the stream header. Returns an error on malformed parameters or
/// a stream that runs out of data.
pub fn decode(data: &[u8], p: &OlympusParams, out: &mut [u16], width: usize, height: usize) -> Result<()> {
    p.validate()?;
    if out.len() < width * height {
        return Err(Error::InvalidArgument("ORF output buffer too small".into()));
    }
    let start = stream_start(data, p)?;
    let mut bits = BitPumpMsb::new(&data[start..]);
    let small_limit = 1u32 << (p.small_shift & 31);
    let grad = 1i32 << p.grad_shift;
    let extra = p.extra_bits;
    let mut ch = [Chan::default(); 2];
    for row in 0..height {
        let (above, cur) = out.split_at_mut(row * width);
        let cur = &mut cur[..width];
        let up = if row >= 2 { Some(&above[(row - 2) * width..(row - 1) * width]) } else { None };
        for col in 0..width {
            let c = &mut ch[col & 1];
            // Adaptive width of the low part from the previous same-channel code.
            let (wbits, run) = if col > 1 {
                let run = if small_limit >= c.prev_code { c.run + 1 } else { 0 };
                let hb = 32 - (c.prev_code & 0xffff).leading_zeros() as i32;
                let (t1, t2) = if run >= p.run_switch {
                    (p.run_threshold, p.run_offset)
                } else {
                    (p.base_threshold, p.base_offset)
                };
                (t2 + (hb - t1).max(-1) + 1, run)
            } else {
                (p.base_offset, 0)
            };
            let wbits = wbits.min(p.max_wbits);
            if !(0..16).contains(&wbits) {
                return Err(Error::corrupt("ORF: invalid code width"));
            }
            let wbits = wbits as u32;
            c.run = run;

            // Entropy decode one code (fill() guarantees 32 buffered bits).
            bits.fill();
            let sign = bits.peek_no_fill(1);
            bits.skip_no_fill(1);
            let lowbits = if p.low_bits > 0 {
                let v = bits.peek_no_fill(p.low_bits);
                bits.skip_no_fill(p.low_bits);
                v
            } else {
                0
            };
            let extrabits = if extra > 0 {
                let v = bits.peek_no_fill(extra);
                bits.skip_no_fill(extra);
                v
            } else {
                0
            };
            let window = bits.peek_no_fill(p.max_high);
            let high = if window != 0 {
                let zeros = window.leading_zeros() - (32 - p.max_high);
                bits.skip_no_fill(zeros + 1);
                zeros
            } else {
                // Escape: `max_high` zeros, then a fixed-width high part and one spare bit.
                bits.skip_no_fill(p.max_high);
                bits.fill();
                let h = if wbits != 15 {
                    let n = 15 - wbits;
                    let v = bits.peek_no_fill(n);
                    bits.skip_no_fill(n);
                    v
                } else {
                    0
                };
                bits.skip_no_fill(1);
                h
            };
            let low = if wbits > 0 {
                bits.fill();
                let v = bits.peek_no_fill(wbits);
                bits.skip_no_fill(wbits);
                v
            } else {
                0
            };
            let glc = low | high.wrapping_shl(wbits);
            let mut code = (glc ^ (sign.wrapping_neg())) as i32;
            c.prev_code = glc;

            // Combine with the smoothed residual.
            let v = if col < 2 { 0 } else { c.smooth };
            let vbits = match p.low_bits {
                0 => 0,
                1 => {
                    code = lowbits as i32 + 2 * (v + code);
                    v + (code >> 1)
                }
                _ => {
                    code = lowbits as i32 + 4 * (v + code);
                    ((code >> 1) & !1) + v + (code >> 2)
                }
            };
            c.smooth = if p.smooth_shift >= 15 { 0 } else { vbits >> p.smooth_shift };

            // Edge-sensitive prediction from same-colour neighbours.
            let w = if col < 2 { p.edge_pred } else { c.west };
            let (mut n, nw) = match up {
                Some(u) => ((u[col] >> extra) as i32, if col < 2 { p.edge_pred } else { (u[col - 2] >> extra) as i32 }),
                None => (p.edge_pred, p.edge_pred),
            };
            if w < n || nw < w {
                if nw <= n && w >= n {
                    n = w;
                } else if nw < n || w >= n {
                    if nw > w || w >= n {
                        if (n - nw).abs() > grad {
                            n += w - nw;
                        } else if (w - nw).abs() <= grad {
                            n = (n + w) >> 1;
                        } else {
                            n = n + w - nw;
                        }
                    }
                } else {
                    n = w;
                }
            }
            let val = code.wrapping_add(n);
            c.west = val;
            cur[col] = (extrabits as i32).wrapping_add(val.wrapping_shl(extra)) as u16;
        }
        if bits.exhausted() {
            return Err(Error::corrupt(format!("ORF: compressed data ends at row {row}/{height}")));
        }
    }
    Ok(())
}

/// Straightforward reference implementation kept for testing the optimised decoder:
/// mirrors the camera's coder description one bit at a time.
#[cfg(test)]
pub(crate) fn decode_reference(data: &[u8], p: &OlympusParams, out: &mut [u16], width: usize, height: usize) {
    let start = stream_start(data, p).unwrap();
    let mut bitpos = start * 8;
    let mut bit = || {
        let b = data.get(bitpos / 8).map(|&b| (b >> (7 - bitpos % 8)) & 1).unwrap_or(0) as u32;
        bitpos += 1;
        b
    };
    let mut ctx = [0i32; 4];
    let (mut lpred, mut pred1, mut glc, mut gcode_prev) = (0i32, 0i32, 0u32, 0u32);
    for row in 0..height {
        for col in 0..width {
            let pcode = gcode_prev;
            gcode_prev = glc;
            let psel = if col > 1 && (1u32 << p.small_shift) >= pcode { ctx[3] + 1 } else { 0 };
            ctx[3] = ctx[2];
            ctx[2] = psel;
            let mut wbits = if col > 1 {
                let hb = 32 - (pcode & 0xffff).leading_zeros() as i32;
                let (t1, t2) = if psel >= p.run_switch {
                    (p.run_threshold, p.run_offset)
                } else {
                    (p.base_threshold, p.base_offset)
                };
                t2 + (hb - t1).max(-1) + 1
            } else {
                p.base_offset
            };
            wbits = wbits.min(p.max_wbits);
            let wbits = wbits as u32;
            let mut rd = |n: u32| (0..n).fold(0u32, |a, _| a << 1 | bit());
            let sign = rd(1);
            let t643 = rd(p.low_bits);
            let t640 = rd(p.extra_bits);
            let mut high = 0;
            loop {
                if rd(1) == 1 {
                    break;
                }
                high += 1;
                if high == p.max_high {
                    high = if wbits != 15 { rd(15 - wbits) } else { 0 };
                    rd(1);
                    break;
                }
            }
            let low = rd(wbits);
            glc = low | (high << wbits);
            let mut gcode = (glc ^ sign.wrapping_neg()) as i32;
            let vbits = if p.low_bits == 2 {
                let v = if col < 2 { 0 } else { pred1 };
                gcode = t643 as i32 + 4 * (v + gcode);
                ((gcode >> 1) & !1) + v + (gcode >> 2)
            } else {
                0
            };
            pred1 = ctx[0];
            ctx[0] = vbits >> p.smooth_shift;
            let w = if col < 2 { p.edge_pred } else { ctx[1] };
            let mut n = if row < 2 { p.edge_pred } else { (out[(row - 2) * width + col] >> p.extra_bits) as i32 };
            let nw = if row < 2 || col < 2 {
                p.edge_pred
            } else {
                (out[(row - 2) * width + col - 2] >> p.extra_bits) as i32
            };
            ctx[1] = lpred;
            if w < n || nw < w {
                if nw <= n && w >= n {
                    n = w;
                } else if nw < n || w >= n {
                    if nw > w || w >= n {
                        if (n - nw).abs() > 1 << p.grad_shift {
                            n += w - nw;
                        } else if (w - nw).abs() <= 1 << p.grad_shift {
                            n = (n + w) >> 1;
                        } else {
                            n = n + w - nw;
                        }
                    }
                } else {
                    n = w;
                }
            }
            lpred = gcode + n;
            out[row * width + col] = (t640 as i32 + ((gcode + n) << p.extra_bits)) as u16;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream(seed: u32, len: usize, header_len: usize) -> Vec<u8> {
        let mut v = vec![0u8; header_len - 1];
        v.push(1);
        v.extend([0, 0]);
        let mut x = seed;
        // Biased towards small codes so the stream exercises runs and escapes.
        v.extend((0..len).map(|i| {
            x = x.wrapping_mul(1664525).wrapping_add(1013904223);
            let b = (x >> 24) as u8;
            if i % 7 == 0 { b & 0x0f } else { b | 0x11 }
        }));
        v
    }

    #[test]
    fn optimised_matches_reference_on_random_streams() {
        for seed in 1..12 {
            let p = OlympusParams::default();
            let data = stream(seed, 40_000, 5);
            let (w, h) = (66, 40);
            let mut a = vec![0u16; w * h];
            let mut b = vec![0u16; w * h];
            let _ = decode(&data, &p, &mut a, w, h);
            decode_reference(&data, &p, &mut b, w, h);
            assert_eq!(a, b, "seed {seed}");
        }
    }

    #[test]
    fn header_and_truncation_checks() {
        let p = OlympusParams::default();
        let mut out = vec![0u16; 64 * 64];
        assert!(decode(&[0, 0, 0, 0, 2, 0, 0, 0xff], &p, &mut out, 64, 64).is_err());
        let data = stream(3, 200, 5);
        assert!(decode(&data, &p, &mut out, 64, 64).is_err());
        let bad = OlympusParams { max_wbits: 40, ..p };
        assert!(decode(&stream(3, 100_000, 5), &bad, &mut out, 64, 64).is_err());
    }
}
