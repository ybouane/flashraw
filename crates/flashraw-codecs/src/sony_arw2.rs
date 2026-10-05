//! Sony ARW2 "cooked" lossy compression.
//!
//! Each sensor row is stored in `width` bytes as a sequence of 16-byte blocks. A
//! block encodes 16 same-colour samples (every other pixel, so two consecutive
//! blocks cover 32 interleaved pixels):
//!
//! ```text
//! bits  0..11  max value (11 bits)       bits 22..26  index of the max sample
//! bits 11..22  min value (11 bits)       bits 26..30  index of the min sample
//! bits 30..128 14 x 7-bit deltas for the remaining samples (LSB-first)
//! ```
//!
//! Deltas are scaled by `1 << sh`, where `sh` is the smallest shift in `0..4` such
//! that `0x80 << sh > max - min`. Decoded 11-bit values are doubled and mapped
//! through the 4096-entry tone curve built from the `SonyRawCurve` (0x7010) tag,
//! producing 14-bit output. Rows are independent, so decoding parallelises per row.

/// Build the 4096-entry ARW2 tone curve from the four `SonyRawCurve` knots.
///
/// Without knots the curve is the identity. Otherwise it is the identity up to the first knot; past each knot the slope
/// doubles (1, 2, 4, 8, 16). Knot values are stored scaled by 4 in the tag.
pub fn build_curve(knots: &[u16]) -> Vec<u16> {
    let mut curve: Vec<u16> = (0..4096u32).map(|i| i as u16).collect();
    if knots.len() < 4 {
        // No curve tag: samples are used as-is.
        return curve;
    }
    let mut pts = [0u32, 0, 0, 0, 0, 4095];
    for (i, k) in knots.iter().take(4).enumerate() {
        pts[i + 1] = ((*k as u32) >> 2) & 0xfff;
    }
    for i in 0..5 {
        let (a, b) = (pts[i] + 1, pts[i + 1]);
        for j in a..=b.min(4095) {
            curve[j as usize] = curve[j as usize - 1].wrapping_add(1 << i);
        }
    }
    curve
}

/// Decode the 16 11-bit samples of one block (`blk` must hold the block plus the two
/// following bytes, which a malformed block with `imax == imin` reads into).
#[inline(always)]
fn decode_block(blk: &[u8; 18], pix: &mut [u16; 16]) {
    let val = u32::from_le_bytes([blk[0], blk[1], blk[2], blk[3]]);
    let max = (val & 0x7ff) as i32;
    let min = ((val >> 11) & 0x7ff) as i32;
    let imax = ((val >> 22) & 0x0f) as usize;
    let imin = ((val >> 26) & 0x0f) as usize;
    let mut sh = 0;
    while sh < 4 && (0x80 << sh) <= max - min {
        sh += 1;
    }
    let mut bit = 30usize;
    if imax != imin {
        // Fast path: all 14 deltas lie inside the 128-bit block.
        #[cfg(not(target_arch = "wasm32"))]
        let v = {
            let mut b16 = [0u8; 16];
            b16.copy_from_slice(&blk[..16]);
            u128::from_le_bytes(b16)
        };
        for (i, p) in pix.iter_mut().enumerate() {
            *p = if i == imax {
                max as u16
            } else if i == imin {
                min as u16
            } else {
                #[cfg(not(target_arch = "wasm32"))]
                let d = ((v >> bit) as u32 & 0x7f) as i32;
                // wasm32 has no 128-bit shifts (each would be a `__lshrti3` call): read the
                // delta from a 16-bit window instead (bit <= 121, so the window ends at byte 16).
                #[cfg(target_arch = "wasm32")]
                let d = ((u16::from_le_bytes([blk[bit >> 3], blk[(bit >> 3) + 1]]) >> (bit & 7)) & 0x7f) as i32;
                bit += 7;
                ((d << sh) + min).min(0x7ff) as u16
            };
        }
    } else {
        // Degenerate block: 15 deltas, the last one straddles the next block's bytes
        // (they are read as part of this block).
        for (i, p) in pix.iter_mut().enumerate() {
            *p = if i == imax {
                max as u16
            } else {
                let w = u16::from_le_bytes([blk[bit >> 3], blk[(bit >> 3) + 1]]);
                let d = ((w >> (bit & 7)) & 0x7f) as i32;
                bit += 7;
                ((d << sh) + min).min(0x7ff) as u16
            };
        }
    }
}

/// Decode one ARW2 row. `src` holds the row's `dst.len()` bytes (shorter input is
/// zero-padded); `curve` is the 4096-entry tone curve from [`build_curve`].
pub fn decode_row(src: &[u8], dst: &mut [u16], curve: &[u16]) {
    let w = dst.len();
    let lut = |p: u16| curve.get((p as usize) << 1).copied().unwrap_or(0);
    let mut col = 0usize;
    let mut off = 0usize;
    let mut pix = [0u16; 16];
    let mut blk = [0u8; 18];
    while col + 30 < w {
        match src.get(off..off + 18) {
            Some(s) => blk.copy_from_slice(s),
            None => {
                blk = [0; 18];
                if let Some(s) = src.get(off..) {
                    let n = s.len().min(18);
                    blk[..n].copy_from_slice(&s[..n]);
                }
            }
        }
        decode_block(&blk, &mut pix);
        for (i, &p) in pix.iter().enumerate() {
            dst[col + 2 * i] = lut(p);
        }
        // Even block covers columns col, col+2, ..; the next block the odd columns.
        col += 32;
        col -= if col & 1 == 1 { 1 } else { 31 };
        off += 16;
    }
}

/// Test helper: encode 16 11-bit samples into a block (exact when `max - min < 128 << sh`
/// and every delta is a multiple of `1 << sh`).
#[cfg(test)]
pub(crate) fn encode_block(pix: &[u16; 16]) -> [u8; 16] {
    let (imax, &max) = pix.iter().enumerate().max_by_key(|&(i, v)| (*v, std::cmp::Reverse(i))).unwrap();
    let (mut imin, &min) = pix.iter().enumerate().min_by_key(|&(i, v)| (*v, i)).unwrap();
    if imin == imax {
        imin = (imax + 1) % 16;
    }
    let mut sh = 0;
    while sh < 4 && (0x80 << sh) <= max as i32 - min as i32 {
        sh += 1;
    }
    let mut v: u128 = max as u128 | (min as u128) << 11 | (imax as u128) << 22 | (imin as u128) << 26;
    let mut bit = 30;
    for (i, &p) in pix.iter().enumerate() {
        if i == imax || i == imin {
            continue;
        }
        let d = ((p - min) >> sh) as u128 & 0x7f;
        v |= d << bit;
        bit += 7;
    }
    v.to_le_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curve_matches_reference_shape() {
        let c = build_curve(&[8000, 10400, 12900, 14100]);
        assert_eq!(c[0], 0);
        assert_eq!(c[2000], 2000);
        assert_eq!(c[2001], 2002);
        assert_eq!(c[2600], 2000 + 600 * 2);
        assert_eq!(c[4095], *c.iter().max().unwrap());
        // Identity curve when no tag.
        let id = build_curve(&[]);
        assert!(id.iter().enumerate().all(|(i, &v)| v as usize == i));
    }

    #[test]
    fn block_roundtrip() {
        let mut seed = 12345u32;
        for sh in 0..4 {
            for _ in 0..200 {
                let mut pix = [0u16; 16];
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                let base = (seed >> 21) as u16 % 1000;
                for p in pix.iter_mut() {
                    seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                    *p = base + (((seed >> 16) as u16 % 0x80) << sh);
                }
                // Force the spread to require exactly `sh`.
                pix[3] = base;
                pix[9] = base + (0x7f << sh).max(1);
                let enc = encode_block(&pix);
                let mut blk = [0u8; 18];
                blk[..16].copy_from_slice(&enc);
                let mut out = [0u16; 16];
                decode_block(&blk, &mut out);
                assert_eq!(out, pix);
            }
        }
    }

    #[test]
    fn row_interleaving() {
        // 64-pixel row: blocks cover even then odd columns of each 32-pixel group.
        let curve = build_curve(&[]);
        let w = 64;
        let mut want = vec![0u16; w];
        let mut src = vec![];
        for g in 0..2 {
            for parity in 0..2 {
                let mut pix = [0u16; 16];
                for (i, p) in pix.iter_mut().enumerate() {
                    *p = (g * 100 + parity * 40 + i * 2) as u16;
                    want[g * 32 + parity + 2 * i] = *p * 2;
                }
                src.extend_from_slice(&encode_block(&pix));
            }
        }
        let mut out = vec![0u16; w];
        decode_row(&src, &mut out, &curve);
        assert_eq!(out, want);
    }

    #[test]
    fn short_input_does_not_panic() {
        let curve = build_curve(&[8000, 10400, 12900, 14100]);
        let mut out = vec![0u16; 128];
        decode_row(&[0xFF; 7], &mut out, &curve);
        decode_row(&[], &mut out, &[]);
        let mut tiny = vec![0u16; 5];
        decode_row(&[1, 2, 3], &mut tiny, &curve);
    }
}
