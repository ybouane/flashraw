//! Self-contained checks for the ARW2 row decoder: deterministic pseudo-random inputs
//! (including degenerate blocks, short sources, odd widths and short curves) are hashed and
//! compared with pinned values, and structural properties are asserted.

use flashraw_codecs::sony_arw2::{build_curve, decode_row};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

fn fnv(h: &mut u64, v: u64) {
    for b in v.to_le_bytes() {
        *h ^= u64::from(b);
        *h = h.wrapping_mul(0x100000001b3);
    }
}

/// Random bytes, with a share of blocks forced to IMAX == IMIN.
fn gen_src(r: &mut Rng, len: usize) -> Vec<u8> {
    let mut v: Vec<u8> = (0..len).map(|_| r.next() as u8).collect();
    for blk in v.chunks_mut(16) {
        if blk.len() == 16 && r.next() % 3 == 0 {
            let idx = (r.next() & 15) as u8;
            // bits 22..25 and 26..29 live in bytes 2 (bits 6,7) and 3 (bits 0..5)
            blk[2] = (blk[2] & 0x3F) | (idx << 6);
            blk[3] = (blk[3] & 0xC0) | (idx >> 2) | (idx << 2);
        }
    }
    v
}

#[test]
fn curve_hash() {
    let mut r = Rng(0x1234_5678_9abc_def1);
    let mut h = 0xcbf29ce484222325u64;
    for n in 0..8usize {
        for _ in 0..40 {
            let knots: Vec<u16> = (0..n).map(|_| r.next() as u16).collect();
            let c = build_curve(&knots);
            assert_eq!(c.len(), 4096);
            for v in c {
                fnv(&mut h, u64::from(v));
            }
        }
    }
    assert_eq!(h, GOLDEN_CURVES, "curve hash {h:#018x}");
}

#[test]
fn row_hash() {
    let mut r = Rng(0x0fed_cba9_8765_4321);
    let mut h = 0xcbf29ce484222325u64;
    let curves =
        [build_curve(&[8000, 10400, 12900, 14100]), build_curve(&[]), build_curve(&[60000, 5, 40000, 9, 1, 2])];
    for case in 0..1500usize {
        let width = (r.next() % 520) as usize;
        let full = (width / 16 + 2) * 16 + 1;
        let len = match case % 4 {
            0 => (r.next() as usize) % (full + 1),
            _ => width,
        };
        let src = gen_src(&mut r, len);
        let mut dst = vec![0xA5A5u16; width];
        let curve: &[u16] = match case % 7 {
            0 => &[],
            1 => &curves[1][..2000],
            k => &curves[k % 3],
        };
        decode_row(&src, &mut dst, curve);
        for v in dst {
            fnv(&mut h, u64::from(v));
        }
    }
    assert_eq!(h, GOLDEN_ROWS, "row hash {h:#018x}");
}

#[test]
fn only_covered_columns_are_written() {
    let mut r = Rng(77);
    let curve = build_curve(&[8000, 10400, 12900, 14100]);
    for width in 0..200usize {
        let src = gen_src(&mut r, width);
        let mut dst = vec![0xBEEFu16; width];
        decode_row(&src, &mut dst, &curve);
        for (c, &v) in dst.iter().enumerate() {
            let (g, rem) = (c / 32, c % 32);
            let covered = rem % 2 == 0 && 32 * g + 30 < width || rem % 2 == 1 && 32 * g + 1 + 30 < width;
            if !covered {
                assert_eq!(v, 0xBEEF, "width {width} col {c}");
            }
        }
    }
}

#[test]
fn curve_is_monotone_for_sorted_knots() {
    let c = build_curve(&[8000, 10400, 12900, 14100]);
    assert!(c.windows(2).all(|w| w[1] >= w[0]));
    assert_eq!(u32::from(c[4095]), u32::from(c[3525]) + (4095 - 3525) * 16);
}

const GOLDEN_CURVES: u64 = 0xe621f2ab9771f51c;
const GOLDEN_ROWS: u64 = 0xbf14bc5f60e29f0c;
