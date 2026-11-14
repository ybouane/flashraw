//! Sony ARW2 lossy row decoder.
//!
//! A stored row is a run of 16-byte blocks. Each block holds 16 samples of one colour plane
//! (every second pixel of a 32-pixel group; odd blocks start one column later). A block is a
//! 128-bit little-endian integer: 11-bit maximum, 11-bit minimum, two 4-bit positions of those
//! extremes, then 7-bit residuals scaled by a power of two. Samples are mapped through a
//! 4096-entry tone curve built from the Sony curve tag.

/// Number of entries in the tone curve.
const CURVE_LEN: usize = 4096;
/// Bytes per block.
const BLOCK: usize = 16;
/// Samples per block.
const SAMPLES: usize = 16;
/// Largest 11-bit sample value.
const MAX11: u32 = 0x7FF;

/// Build the 4096-entry ARW2 tone curve from the curve-tag knots.
///
/// With fewer than four knots the curve is the identity. Otherwise five consecutive segments
/// with slopes 1, 2, 4, 8 and 16 are laid over the breakpoints `knot >> 2` (12-bit domain).
pub fn build_curve(knots: &[u16]) -> Vec<u16> {
    let mut curve: Vec<u16> = (0..CURVE_LEN as u16).collect();
    if knots.len() < 4 {
        return curve;
    }
    let mut bp = [0usize; 6];
    for (b, k) in bp[1..5].iter_mut().zip(knots) {
        *b = usize::from((*k >> 2) & 0xFFF);
    }
    bp[5] = CURVE_LEN - 1;
    for s in 0..5 {
        let step = 1u16 << s;
        let end = bp[s + 1].min(CURVE_LEN - 1);
        for j in bp[s] + 1..=end {
            curve[j] = curve[j - 1].wrapping_add(step);
        }
    }
    curve
}

/// Decode one ARW2 row from `src` into `dst` (`dst.len()` is the row width) through `curve`.
///
/// Only columns covered by whole blocks are written. Missing source bytes read as zero and a
/// curve shorter than needed yields zero samples; nothing here panics or reports an error.
pub fn decode_row(src: &[u8], dst: &mut [u16], curve: &[u16]) {
    let width = dst.len();
    if width < 31 {
        return;
    }
    let table: Option<&[u16; CURVE_LEN]> = curve.get(..CURVE_LEN).and_then(|c| c.try_into().ok());
    let groups = (width - 31) / 32 + 1; // groups whose even block fits
    let mut off = 0usize;
    for g in 0..groups {
        for p in 0..2usize {
            let c0 = 32 * g + p;
            if c0 + 30 >= width {
                break;
            }
            let out = &mut dst[c0..c0 + 31];
            let x = match src.get(off..off + BLOCK + 1) {
                Some(b) => {
                    let mut w = [0u8; BLOCK];
                    w.copy_from_slice(&b[..BLOCK]);
                    (u128::from_le_bytes(w), b[BLOCK])
                }
                None => load_tail(src, off),
            };
            match table {
                Some(t) => block_fast(x.0, x.1, out, t),
                None => block_slow(x.0, x.1, out, curve),
            }
            off += BLOCK;
        }
    }
}

/// Read a block and its following byte when the source ends early (missing bytes are zero).
#[cold]
fn load_tail(src: &[u8], off: usize) -> (u128, u8) {
    let mut w = [0u8; BLOCK + 1];
    if off < src.len() {
        let n = (src.len() - off).min(BLOCK + 1);
        w[..n].copy_from_slice(&src[off..off + n]);
    }
    let mut b = [0u8; BLOCK];
    b.copy_from_slice(&w[..BLOCK]);
    (u128::from_le_bytes(b), w[BLOCK])
}

/// Header fields of a block: (max, min, imax, imin, shift).
#[inline(always)]
fn header(x: u128) -> (u32, u32, u32, u32, u32) {
    let max = (x & 0x7FF) as u32;
    let min = ((x >> 11) & 0x7FF) as u32;
    let imax = ((x >> 22) & 15) as u32;
    let imin = ((x >> 26) & 15) as u32;
    let d = max as i32 - min as i32;
    let sh = u32::from(d >= 0x80) + u32::from(d >= 0x100) + u32::from(d >= 0x200) + u32::from(d >= 0x400);
    (max, min, imax, imin, sh)
}

/// The 16 sample values (11-bit domain) of a block.
///
/// A block whose two extreme positions coincide has only one special position and a 15th
/// residual in the following byte; both cases share one branch-free walk.
#[inline(always)]
fn block_values(x: u128, next: u8) -> [u32; SAMPLES] {
    let (max, min, imax, imin, sh) = header(x);
    // With equal positions the minimum is not emitted: park its position out of range.
    let imin = if imin == imax { 16 } else { imin };
    // Residuals in stream order; the 15th (only used when the positions coincide) is the low
    // seven bits of the following byte.
    let rest = x >> 30;
    let mut d = [0u32; SAMPLES];
    for (k, slot) in d.iter_mut().take(14).enumerate() {
        let r = ((rest >> (7 * k)) & 0x7F) as u32;
        *slot = ((r << sh) + min).min(MAX11);
    }
    d[14] = ((u32::from(next & 0x7F) << sh) + min).min(MAX11);
    let mut v = [0u32; SAMPLES];
    for (i, slot) in v.iter_mut().enumerate() {
        let iu = i as u32;
        // Residual number = position minus the special positions before it.
        let k = (iu - u32::from(imax < iu) - u32::from(imin < iu)) as usize & 15;
        let mut s = d[k];
        if iu == imin {
            s = min;
        }
        if iu == imax {
            s = max;
        }
        *slot = s;
    }
    v
}

#[inline(always)]
fn block_fast(x: u128, next: u8, out: &mut [u16], t: &[u16; CURVE_LEN]) {
    let v = block_values(x, next);
    for (i, &s) in v.iter().enumerate() {
        out[2 * i] = t[(2 * s as usize) & (CURVE_LEN - 1)];
    }
}

#[cold]
fn block_slow(x: u128, next: u8, out: &mut [u16], curve: &[u16]) {
    let v = block_values(x, next);
    for (i, &s) in v.iter().enumerate() {
        out[2 * i] = curve.get(2 * s as usize).copied().unwrap_or(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curve_example() {
        let c = build_curve(&[8000, 10400, 12900, 14100]);
        assert_eq!(c.len(), 4096);
        assert_eq!(c[2000], 2000);
        assert_eq!(c[2001], 2002);
        assert_eq!(c[2600], 3200);
        assert_eq!(c[2601], 3204);
        assert_eq!(build_curve(&[1, 2, 3])[1234], 1234);
    }

    /// Test-only encoder for normal blocks.
    fn encode(vals: &[u32; 16], imax: usize, imin: usize, sh: u32) -> [u8; 16] {
        let max = vals[imax];
        let min = vals[imin];
        let mut x: u128 = u128::from(max) | (u128::from(min) << 11) | ((imax as u128) << 22) | ((imin as u128) << 26);
        let mut k = 0;
        for (i, &v) in vals.iter().enumerate() {
            if i == imax || i == imin {
                continue;
            }
            x |= u128::from((v - min) >> sh) << (30 + 7 * k);
            k += 1;
        }
        x.to_le_bytes()
    }

    fn identity() -> Vec<u16> {
        build_curve(&[])
    }

    #[test]
    fn block_round_trip() {
        let vals: [u32; 16] = [100, 130, 90, 200, 150, 160, 170, 180, 140, 110, 120, 190, 105, 115, 125, 135];
        let (imax, imin) = (3, 2);
        let bytes = encode(&vals, imax, imin, 0);
        let mut dst = vec![0u16; 32];
        decode_row(&bytes, &mut dst, &identity());
        for i in 0..16 {
            assert_eq!(u32::from(dst[2 * i]), 2 * vals[i]);
            assert_eq!(dst[2 * i + 1], 0);
        }
    }

    #[test]
    fn shift_scales_residuals() {
        let mut vals = [0u32; 16];
        for (i, v) in vals.iter_mut().enumerate() {
            *v = 16 * i as u32; // all multiples of 4, range 240 -> shift 1
        }
        let bytes = encode(&vals, 15, 0, 1);
        let mut dst = vec![0u16; 32];
        decode_row(&bytes, &mut dst, &identity());
        for i in 0..16 {
            assert_eq!(u32::from(dst[2 * i]), 2 * vals[i]);
        }
    }

    #[test]
    fn odd_block_is_interleaved() {
        let mut src = vec![0u8; 32];
        src[..16].copy_from_slice(&encode(&[10; 16].map(|v| v + 1), 0, 1, 0));
        src[16..].copy_from_slice(&encode(&[20; 16].map(|v| v + 1), 0, 1, 0));
        let mut dst = vec![0u16; 32];
        decode_row(&src, &mut dst, &identity());
        for c in 0..32 {
            assert_eq!(dst[c], if c % 2 == 0 { 22 } else { 42 });
        }
    }

    #[test]
    fn degenerate_uses_following_byte() {
        // MAX=MIN=5 at index 0, all residuals zero except the 15th, held in the next byte.
        let mut b = [0u8; 17];
        let x: u128 = 5 | (5 << 11);
        b[..16].copy_from_slice(&x.to_le_bytes());
        b[16] = 0x83; // low 7 bits = 3
        let mut dst = vec![0u16; 32];
        decode_row(&b, &mut dst, &identity());
        assert_eq!(dst[0], 10);
        assert_eq!(dst[2 * 15], 16); // (3 << 0) + 5 = 8 -> doubled
        assert_eq!(dst[2], 10);
        // Without the extra byte it reads as zero.
        decode_row(&b[..16], &mut dst, &identity());
        assert_eq!(dst[30], 10);
    }

    #[test]
    fn short_inputs_do_not_panic_and_leave_other_columns() {
        let mut dst = vec![7u16; 70];
        decode_row(&[], &mut dst, &[]);
        assert_eq!(dst[0], 0);
        assert_eq!(dst[1], 0);
        assert_eq!(dst[64], 7);
        assert_eq!(dst[69], 7);
        let mut small = vec![9u16; 20];
        decode_row(&[1, 2, 3], &mut small, &identity());
        assert!(small.iter().all(|&v| v == 9));
    }
}
