//! Phase One IIQ entropy coders.
//!
//! IIQ stores a row-offset table, so every row is an independent bit stream (the
//! container decodes rows in parallel). Two coders exist:
//!
//! - **IIQ L** ("raw_c", formats 3/5/8): per 8-pixel block and per column parity a
//!   prefix code selects a difference width from a 10-entry table; width 14 is an
//!   escape for a literal 16-bit value. Samples are 14-bit (shifted left by two into
//!   the 16-bit range) except for format 8, which carries native 16-bit values.
//!   Format 5 additionally maps values below 256 through a square-law curve.
//! - **IIQ S** (format 6): per 8-pixel block, each parity keeps a "bit check" level
//!   (adjusted by a delta code or reset through an index table) that sets the
//!   quantisation step of the coded differences; level 9 is a literal escape.
//!
//! Both read bits MSB-first from 32-bit little-endian words ([`BitPumpMsb32`]) for
//! little-endian files (the big-endian variant uses [`BitPumpMsb`]).
//!
//! [`BitPumpMsb32`]: flashraw_core::bits::BitPumpMsb32
//! [`BitPumpMsb`]: flashraw_core::bits::BitPumpMsb

use flashraw_core::bits::BitRead;
use flashraw_core::{Error, Result};

/// Difference widths selected by the IIQ L prefix code.
const L_LENGTHS: [u32; 10] = [8, 7, 6, 9, 11, 10, 5, 12, 14, 13];

/// Variant parameters of the IIQ L coder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IiqLParams {
    /// Shift applied to decoded values (2 for 14-bit data, 0 for format 8).
    pub shift: u32,
    /// Format 5: values below 256 go through the square-law curve.
    pub low_curve: bool,
}

/// Format-5 low-range curve: `round(i^2 / 3.969)` (as single-precision floats, matching
/// the reference implementation bit for bit).
fn low_curve_table() -> [u16; 256] {
    let mut t = [0u16; 256];
    for (i, v) in t.iter_mut().enumerate() {
        *v = ((i * i) as f32 / 3.969f32 + 0.5f32) as u16;
    }
    t
}

/// Decode one IIQ L row of `out.len()` samples from a bit pump positioned at the row start.
///
/// (out-of-range sums wrap when stored), and blocks whose prefix is a single `1` bit
/// keep the previous width.
#[inline(never)]
pub fn decode_l_row<B: BitRead>(bits: &mut B, out: &mut [u16], p: IiqLParams) -> Result<()> {
    let width = out.len();
    let full_blocks = width & !7;
    let curve = if p.low_curve { Some(low_curve_table()) } else { None };
    let mut len = [0u32; 2];
    let mut pred = [0i32; 2];
    for (col, o) in out.iter_mut().enumerate() {
        if col >= full_blocks {
            len = [14, 14];
        } else if col & 7 == 0 {
            for l in len.iter_mut() {
                // Up to five zero bits terminated by a one; a non-zero count selects a
                // length together with one more bit. One peek replaces the bit loop.
                bits.fill();
                let peek = bits.peek_no_fill(7);
                let top = peek >> 2;
                let zeros = if top == 0 { 5 } else { top.leading_zeros() as usize - 27 };
                let mut used = if zeros < 5 { zeros as u32 + 1 } else { 5 };
                if zeros > 0 {
                    let extra = (peek >> (6 - used)) & 1;
                    used += 1;
                    *l = L_LENGTHS[(zeros - 1) * 2 + extra as usize];
                }
                bits.skip_no_fill(used);
            }
        }
        let parity = col & 1;
        let w = len[parity];
        let v = match w {
            14 => {
                let v = bits.get(16) as i32;
                pred[parity] = v;
                v
            }
            0 => return Err(Error::corrupt("IIQ L: block without difference width")),
            _ => {
                let d = bits.get(w) as i32 + 1 - (1 << (w - 1));
                pred[parity] = pred[parity].wrapping_add(d);
                pred[parity]
            }
        };
        let mut s = v as u16;
        if let Some(c) = &curve {
            if s < 256 {
                s = c[s as usize];
            }
        }
        *o = s << p.shift;
    }
    if bits.exhausted() {
        return Err(Error::corrupt("IIQ L: row data truncated"));
    }
    Ok(())
}

const S_USED_CORR: [u32; 8] = [3, 3, 3, 3, 1, 1, 1, 1];
const S_EXTRA_BITS: [u32; 8] = [1, 2, 3, 4, 0, 0, 0, 0];
const S_BIT_INDICATOR: [u32; 32] =
    [9, 8, 0, 7, 6, 6, 5, 5, 1, 1, 1, 1, 4, 4, 4, 4, 3, 3, 3, 3, 3, 3, 3, 3, 2, 2, 2, 2, 2, 2, 2, 2];
const S_SKIP_BITS: [u32; 32] =
    [5, 5, 5, 5, 4, 4, 4, 4, 3, 3, 3, 3, 3, 3, 3, 3, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2];

/// this for out-of-range levels in malformed blocks; emulating it keeps output
/// identical without undefined behaviour.
#[inline(always)]
fn shl32(v: u32, s: u32) -> u32 {
    v.wrapping_shl(s)
}

/// Read the level update of one parity in an IIQ S block.
#[inline(always)]
fn s_level<B: BitRead>(bits: &mut B, level: &mut u32) {
    let idx = bits.peek(7);
    bits.skip(2);
    if idx >= 32 {
        *level = (idx >> 5).wrapping_add(*level).wrapping_sub(2);
    } else {
        *level = S_BIT_INDICATOR[idx as usize];
        bits.skip(S_SKIP_BITS[idx as usize]);
    }
}

pub fn decode_s_row<B: BitRead>(bits: &mut B, out: &mut [u16]) -> Result<()> {
    let width = out.len();
    let init_bits = bits.get(16) & 7;
    let blocks = width / 8;
    let mut level = [0u32; 2];
    let mut prev = [0i32; 2];
    let pix_sub_init = 17u32.wrapping_sub(init_bits);
    for blk in out[..blocks * 8].chunks_exact_mut(8) {
        s_level(bits, &mut level[0]);
        s_level(bits, &mut level[1]);
        let bidx = bits.peek(3) as usize;
        bits.skip(S_USED_CORR[bidx]);
        let extra = S_EXTRA_BITS[bidx];
        let take = init_bits + extra;
        let shift = [level[0].wrapping_sub(extra), level[1].wrapping_sub(extra)];
        let sub = [
            0xFFFFu32.wrapping_shr(pix_sub_init.wrapping_sub(level[0])) as i32,
            0xFFFFu32.wrapping_shr(pix_sub_init.wrapping_sub(level[1])) as i32,
        ];
        for (i, o) in blk.iter_mut().enumerate() {
            let p = i & 1;
            let value = if level[p] == 9 {
                bits.get(14) as i32
            } else {
                let coded = if take == 0 { 0 } else { bits.get(take) };
                prev[p].wrapping_add(shl32(coded, shift[p]) as i32).wrapping_sub(sub[p])
            };
            *o = value.wrapping_shl(2).clamp(0, 0xFFFF) as u16;
            prev[p] = value;
        }
    }
    for o in out[blocks * 8..].iter_mut() {
        *o = (bits.get(14) << 2) as u16;
    }
    if bits.exhausted() {
        return Err(Error::corrupt("IIQ S: row data truncated"));
    }
    Ok(())
}

/// Undo the key scrambling of uncompressed IIQ formats 1 and 2 on a run of samples
/// (processed in pairs over the whole raster, independent of row boundaries).
pub fn unscramble_pairs(samples: &mut [u16], akey: u16, bkey: u16, format: u32) {
    let mask: u16 = if format == 1 { 0x5555 } else { 0x1354 };
    for pair in samples.chunks_exact_mut(2) {
        let a = pair[0] ^ akey;
        let b = pair[1] ^ bkey;
        pair[0] = (a & mask) | (b & !mask);
        pair[1] = (b & mask) | (a & !mask);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flashraw_core::bits::BitPumpMsb32;

    /// MSB-first bit writer emitting 32-bit little-endian words.
    #[derive(Default)]
    pub struct Writer {
        words: Vec<u32>,
        acc: u64,
        n: u32,
    }
    impl Writer {
        pub fn put(&mut self, v: u32, bits: u32) {
            if bits == 0 {
                return;
            }
            self.acc = (self.acc << bits) | (v as u64 & ((1u64 << bits) - 1));
            self.n += bits;
            while self.n >= 32 {
                self.words.push((self.acc >> (self.n - 32)) as u32);
                self.n -= 32;
            }
        }
        pub fn finish(mut self) -> Vec<u8> {
            if self.n > 0 {
                self.words.push((self.acc << (32 - self.n)) as u32);
            }
            self.words.push(0);
            self.words.iter().flat_map(|w| w.to_le_bytes()).collect()
        }
    }

    /// Reference IIQ L encoder (14-bit samples, shift 2).
    fn encode_l(samples: &[u16]) -> Vec<u8> {
        let mut w = Writer::default();
        let width = samples.len();
        let mut pred = [0i32; 2];
        let mut len = [0u32; 2];
        for col in 0..width {
            if col >= width & !7 {
                len = [14, 14];
            } else if col & 7 == 0 {
                for p in 0..2 {
                    let mut need = 0;
                    let mut pr = pred[p];
                    for c in (col + p..col + 8).step_by(2) {
                        let d = samples[c] as i32 - pr;
                        pr = samples[c] as i32;
                        need = need.max(d.abs() + 1);
                    }
                    let mut best = 8; // index of width 14
                    for (k, &l) in L_LENGTHS.iter().enumerate() {
                        if l != 14 && (1 << (l - 1)) > need && L_LENGTHS[k] < L_LENGTHS[best] {
                            best = k;
                        }
                    }
                    let j = best / 2;
                    for _ in 0..=j {
                        w.put(0, 1);
                    }
                    if j + 1 < 5 {
                        w.put(1, 1);
                    }
                    w.put((best & 1) as u32, 1);
                    len[p] = L_LENGTHS[best];
                }
            }
            let p = col & 1;
            let v = samples[col] as i32;
            if len[p] == 14 {
                w.put(v as u32, 16);
            } else {
                let l = len[p];
                w.put((v - pred[p] - 1 + (1 << (l - 1))) as u32, l);
            }
            pred[p] = v;
        }
        w.finish()
    }

    #[test]
    fn l_round_trip() {
        let mut seed = 12345u32;
        for width in [1usize, 7, 8, 9, 16, 63, 200] {
            let samples: Vec<u16> = (0..width)
                .map(|i| {
                    seed = seed.wrapping_mul(1103515245).wrapping_add(12345);
                    let base = 2000 + (i as i32 * 37 % 900);
                    (base + (seed >> 20) as i32 % 300) as u16 & 0x3FFF
                })
                .collect();
            let data = encode_l(&samples);
            let mut out = vec![0u16; width];
            decode_l_row(&mut BitPumpMsb32::new(&data), &mut out, IiqLParams { shift: 2, low_curve: false }).unwrap();
            let expect: Vec<u16> = samples.iter().map(|v| v << 2).collect();
            assert_eq!(out, expect, "width {width}");
        }
    }

    #[test]
    fn l_format8_and_curve() {
        let samples: Vec<u16> = vec![10, 300, 255, 40000, 3, 2, 1, 0, 100];
        let data = encode_l(&samples);
        let mut out = vec![0u16; samples.len()];
        decode_l_row(&mut BitPumpMsb32::new(&data), &mut out, IiqLParams { shift: 0, low_curve: false }).unwrap();
        assert_eq!(out, samples);
        decode_l_row(&mut BitPumpMsb32::new(&data), &mut out, IiqLParams { shift: 2, low_curve: true }).unwrap();
        let c = low_curve_table();
        assert_eq!(out[0], c[10] << 2);
        assert_eq!(out[1], 300 << 2);
        assert_eq!(c[255], 16383);
    }

    #[test]
    fn l_truncated_is_error() {
        let samples: Vec<u16> = (0..64).map(|i| (i * 211 % 16000) as u16).collect();
        let data = encode_l(&samples);
        let mut out = vec![0u16; 64];
        let r = decode_l_row(&mut BitPumpMsb32::new(&data[..8]), &mut out, IiqLParams { shift: 2, low_curve: false });
        assert!(r.is_err());
    }

    /// IIQ S: random level updates and correction modes; expected output follows the
    /// documented value formula independently of the bit parsing.
    #[test]
    fn s_random_stream() {
        let mut seed = 99u32;
        let mut rnd = |n: u32| {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            (seed >> 8) % n
        };
        for width in [8usize, 16, 27, 64] {
            let init_bits = 3u32;
            let mut w = Writer::default();
            w.put(init_bits, 16);
            let mut expect = vec![0u16; width];
            let mut level = [0u32; 2];
            let mut prev = [0i32; 2];
            for blk in 0..width / 8 {
                for lv in level.iter_mut() {
                    // Either reset through the index table or apply a delta (when valid).
                    if blk == 0 || !(6..=7).contains(&*lv) || rnd(2) == 0 {
                        let target = [9u32, 8, 7, 6, 5][rnd(5) as usize];
                        let idx = S_BIT_INDICATOR.iter().position(|&b| b == target).unwrap() as u32;
                        let skip = S_SKIP_BITS[idx as usize];
                        w.put(idx >> (5 - skip), 2 + skip);
                        *lv = target;
                    } else {
                        let top = 1 + rnd(3); // 1..=3 -> delta -1..=+1
                        w.put(top, 2);
                        *lv = *lv + top - 2;
                    }
                }
                let bidx = rnd(8);
                if bidx < 4 {
                    w.put(bidx, 3);
                } else {
                    w.put(1, 1);
                }
                let extra = S_EXTRA_BITS[bidx as usize];
                let take = init_bits + extra;
                for i in 0..8 {
                    let p = i & 1;
                    let value = if level[p] == 9 {
                        let v = rnd(1 << 14);
                        w.put(v, 14);
                        v as i32
                    } else {
                        let coded = rnd(1 << take);
                        w.put(coded, take);
                        let sub = (0xFFFFu32 >> (17 - init_bits - level[p])) as i32;
                        prev[p] + ((coded << (level[p] - extra)) as i32) - sub
                    };
                    expect[blk * 8 + i] = (value << 2).clamp(0, 0xFFFF) as u16;
                    prev[p] = value;
                }
            }
            for e in expect.iter_mut().skip(width / 8 * 8) {
                let v = rnd(1 << 14);
                w.put(v, 14);
                *e = (v << 2) as u16;
            }
            let data = w.finish();
            let mut out = vec![0u16; width];
            decode_s_row(&mut BitPumpMsb32::new(&data), &mut out).unwrap();
            assert_eq!(out, expect, "width {width}");
        }
    }

    #[test]
    fn s_garbage_does_not_panic() {
        let mut seed = 7u32;
        for len in [0usize, 3, 17, 300] {
            let data: Vec<u8> = (0..len)
                .map(|_| {
                    seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                    (seed >> 24) as u8
                })
                .collect();
            let mut out = vec![0u16; 100];
            let _ = decode_s_row(&mut BitPumpMsb32::new(&data), &mut out);
            let _ = decode_l_row(&mut BitPumpMsb32::new(&data), &mut out, IiqLParams { shift: 2, low_curve: true });
        }
    }

    #[test]
    fn unscramble_is_involution_for_mask_swap() {
        let orig = [0x1234u16, 0xABCD, 0xFFFF, 0x0000];
        let mut s = orig;
        unscramble_pairs(&mut s, 0, 0, 1);
        // With zero keys, applying twice swaps the masked bits back.
        unscramble_pairs(&mut s, 0, 0, 1);
        assert_eq!(s, orig);
    }
}
