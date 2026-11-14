//! Tests for the headerless-dump unpackers: deterministic property tests, plus golden hashes of
//! the output over a fixed pseudo-random case set (pinning the behaviour on layouts no corpus
//! file exercises: pad bytes, interlace, 24-bit words, short rows, truncation, odd widths).

use flashraw_codecs::misc_packed::{
    PackedParams, loose10_rows, mipi10_rows, packed_rows, packed_rows_reference, u16_rows,
};

pub const SENTINEL: u16 = 0xA5A5;

pub struct Rng(pub u64);
impl Rng {
    pub fn step(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    pub fn below(&mut self, n: usize) -> usize {
        (self.step() % n as u64) as usize
    }
    pub fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| (self.step() >> 24) as u8).collect()
    }
}

pub struct PackedCase {
    pub data: Vec<u8>,
    pub offset: usize,
    pub p: PackedParams,
    pub out_len: usize,
}

/// Random generic-row cases: all flag combinations, every bit depth, complete, truncated, long
/// and offset-past-end payloads.
pub fn packed_cases(seed: u64, n: usize) -> Vec<PackedCase> {
    let mut r = Rng(seed);
    (0..n)
        .map(|_| {
            let width = 1 + r.below(70);
            let height = 1 + r.below(9);
            let bits = 1 + r.below(16) as u32;
            let flags = r.below(256) as u32;
            let p = PackedParams { width, height, bits, flags };
            let need = p.row_bytes() * height + 16;
            let len = match r.below(5) {
                0 => need,
                1 => r.below(need + 1),
                2 => need * 2,
                3 => r.below(need * 2 + 1),
                _ => need + r.below(8),
            };
            let offset = match r.below(6) {
                0 => r.below(len + 5),
                1 => r.below(9),
                _ => 0,
            };
            let out_len = if r.below(8) == 0 { width * height + r.below(5) } else { width * height };
            PackedCase { data: r.bytes(len), offset, p, out_len }
        })
        .collect()
}

/// Cases drawn only from layouts the row-independent decoder handles, with larger geometry.
pub fn fast_cases(seed: u64, n: usize) -> Vec<PackedCase> {
    let mut r = Rng(seed);
    (0..n)
        .map(|_| {
            let width = 1 + r.below(300);
            let height = 1 + r.below(12);
            let bits = [8u32, 10, 12, 14, 16, 9, 11, 13, 15, 1, 5][r.below(11)];
            let flags = [0u32, 8, 24, 64, 72, 88, 128, 136, 152, 168, 216][r.below(11)];
            let p = PackedParams { width, height, bits, flags };
            let need = p.row_bytes() * height;
            let len = match r.below(4) {
                0 => need,
                1 => r.below(need + 1),
                2 => need + r.below(40),
                _ => (r.below(need + 1) / 4) * 4,
            };
            let offset = if r.below(3) == 0 { r.below(64) } else { 0 };
            PackedCase { data: r.bytes(len + offset), offset, p, out_len: width * height }
        })
        .collect()
}

pub struct Fnv(pub u64);
impl Default for Fnv {
    fn default() -> Self {
        Fnv(0xcbf29ce484222325)
    }
}
impl Fnv {
    pub fn add(&mut self, cells: &[u16]) {
        for &c in cells {
            for b in c.to_le_bytes() {
                self.0 = (self.0 ^ u64::from(b)).wrapping_mul(0x100000001b3);
            }
        }
    }
}

pub fn run_packed(c: &PackedCase, parallel: bool) -> (bool, Vec<u16>) {
    let mut out = vec![SENTINEL; c.out_len];
    let ok = packed_rows(&c.data, c.offset, c.p, &mut out, parallel).is_ok();
    (ok, out)
}

pub fn run_reference(c: &PackedCase) -> Vec<u16> {
    let mut out = vec![SENTINEL; c.out_len];
    packed_rows_reference(&c.data, c.offset, c.p, &mut out);
    out
}

pub struct SmallCase {
    pub data: Vec<u8>,
    pub w: usize,
    pub h: usize,
    pub stride: usize,
    pub be: bool,
    pub shift: u32,
    pub bottom_up: bool,
}

pub fn small_cases(seed: u64, n: usize) -> Vec<SmallCase> {
    let mut r = Rng(seed);
    (0..n)
        .map(|_| {
            let w = 1 + r.below(40);
            let h = 1 + r.below(7);
            let need = 2 * w * h + 8;
            let len = match r.below(4) {
                0 => need,
                1 => r.below(need + 1),
                2 => need + r.below(30),
                _ => r.below(2 * need),
            };
            SmallCase {
                data: r.bytes(len),
                w,
                h,
                stride: r.below(w.div_ceil(4) * 5 * 2 + 3),
                be: r.below(2) == 0,
                shift: r.below(20) as u32,
                bottom_up: r.below(2) == 0,
            }
        })
        .collect()
}

pub fn run_mipi(c: &SmallCase, parallel: bool) -> Vec<u16> {
    let mut out = vec![SENTINEL; c.w * c.h];
    mipi10_rows(&c.data, c.w, c.h, c.stride, &mut out, parallel);
    out
}
pub fn run_loose(c: &SmallCase, parallel: bool) -> Vec<u16> {
    let mut out = vec![SENTINEL; c.w * c.h];
    loose10_rows(&c.data, c.w, c.h, &mut out, parallel);
    out
}
pub fn run_u16(c: &SmallCase, parallel: bool) -> Vec<u16> {
    let mut out = vec![SENTINEL; c.w * c.h];
    u16_rows(&c.data, c.w, c.h, c.be, c.shift, c.bottom_up, &mut out, parallel);
    out
}

/// Hashes of the outputs over the fixed case sets: (generic, reference, fast, mipi, loose, u16).
pub fn golden_hashes() -> [u64; 6] {
    let (mut g, mut rf, mut f, mut m, mut l, mut u) =
        (Fnv::default(), Fnv::default(), Fnv::default(), Fnv::default(), Fnv::default(), Fnv::default());
    for c in packed_cases(0x1234_5678_9abc_def1, 6000) {
        let (ok, out) = run_packed(&c, false);
        g.add(&[u16::from(ok)]);
        g.add(&out);
        rf.add(&run_reference(&c));
    }
    for c in fast_cases(0x0bad_cafe_f00d_1234, 3000) {
        f.add(&run_packed(&c, false).1);
    }
    for c in small_cases(0x5555_aaaa_1357_9bdf, 3000) {
        m.add(&run_mipi(&c, false));
        l.add(&run_loose(&c, false));
        u.add(&run_u16(&c, false));
    }
    [g.0, rf.0, f.0, m.0, l.0, u.0]
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOLDEN: [u64; 6] = [
        0x6b6b3a3dd1688bf7,
        0xae632575afbbd85a,
        0xf9ed0cc2482c2724,
        0xf65a69d31016578e,
        0xe690b9e34a88c1da,
        0x04bfcc142dab9b09,
    ];

    #[test]
    fn golden_outputs_are_stable() {
        let h = golden_hashes();
        assert_eq!(h, GOLDEN, "{h:#018x?}");
    }

    #[test]
    fn parallel_flag_never_changes_output() {
        for c in fast_cases(77, 300) {
            assert_eq!(run_packed(&c, false), run_packed(&c, true));
        }
        for c in small_cases(78, 300) {
            assert_eq!(run_mipi(&c, false), run_mipi(&c, true));
            assert_eq!(run_loose(&c, false), run_loose(&c, true));
            assert_eq!(run_u16(&c, false), run_u16(&c, true));
        }
    }

    #[test]
    fn independent_layouts_equal_reference_when_complete() {
        // Whole-word rows, complete input, even width: both models agree.
        for flags in [0u32, 8, 24, 64, 128, 136, 168, 88] {
            for bits in 1..=16u32 {
                let (w, h) = (48usize, 5usize);
                let p = PackedParams { width: w, height: h, bits, flags };
                if (p.row_bytes() * 8) % (8 + (flags & 24)) as usize != 0 || p.row_bytes() * 8 < w * bits as usize {
                    continue;
                }
                let data = Rng(bits as u64 * 31 + flags as u64 + 1).bytes(p.row_bytes() * h + 8);
                let mut a = vec![0u16; w * h];
                let mut b = vec![0u16; w * h];
                packed_rows(&data, 0, p, &mut a, false).unwrap();
                packed_rows_reference(&data, 0, p, &mut b);
                assert_eq!(a, b, "bits {bits} flags {flags}");
            }
        }
    }

    #[test]
    fn known_layouts() {
        // 12-bit in 16-bit words: bytes A B C D E F -> words B:A, D:C, F:E.
        let data = [0x12u8, 0x34, 0x56, 0x78, 0x9a, 0xbc];
        let p = PackedParams { width: 4, height: 1, bits: 12, flags: 8 };
        let mut out = [0u16; 4];
        packed_rows(&data, 0, p, &mut out, false).unwrap();
        assert_eq!(out, [0x341, 0x278, 0x56b, 0xc9a]);
        // Plain big-endian 12-bit.
        let p = PackedParams { width: 2, height: 1, bits: 12, flags: 0 };
        let mut out = [0u16; 2];
        packed_rows(&[0xab, 0xcd, 0xef], 0, p, &mut out, false).unwrap();
        assert_eq!(out, [0xabc, 0xdef]);
        // MIPI RAW10.
        let mut out = [0u16; 4];
        mipi10_rows(&[1, 2, 3, 4, 0b11_10_01_00], 4, 1, 5, &mut out, false);
        assert_eq!(out, [4, 9, 14, 19]);
        // Loose RAW10.
        let word: u64 = (1..=6u64).enumerate().map(|(i, v)| (v * 100) << (10 * i)).sum();
        let mut out = [0u16; 6];
        loose10_rows(&word.to_le_bytes(), 6, 1, &mut out, false);
        assert_eq!(out, [100, 200, 300, 400, 500, 600]);
        // 16-bit rows, bottom-up with shift.
        let src = [0x00, 0x10, 0x00, 0x20, 0x00, 0x30, 0x00, 0x40];
        let mut out = [0u16; 4];
        u16_rows(&src, 2, 2, true, 4, true, &mut out, false);
        assert_eq!(out, [0x03, 0x04, 0x01, 0x02]);
    }

    #[test]
    fn geometry_errors() {
        let mut out = [0u16; 4];
        for p in [
            PackedParams { width: 2, height: 2, bits: 0, flags: 0 },
            PackedParams { width: 2, height: 2, bits: 17, flags: 0 },
            PackedParams { width: 0, height: 2, bits: 8, flags: 0 },
            PackedParams { width: 3, height: 2, bits: 8, flags: 0 },
        ] {
            assert!(packed_rows(&[0; 16], 0, p, &mut out, false).is_err());
        }
        let p = PackedParams { width: 2, height: 0, bits: 8, flags: 0 };
        assert!(packed_rows(&[], 0, p, &mut out, false).is_ok());
    }

    #[test]
    fn offset_past_end_and_truncation() {
        let p = PackedParams { width: 8, height: 3, bits: 8, flags: 0 };
        let mut out = vec![SENTINEL; 24];
        packed_rows(&[1u8; 10], 50, p, &mut out, false).unwrap();
        assert!(out[..8].iter().all(|&v| v == 0) && out[8..].iter().all(|&v| v == SENTINEL));
        let mut out = vec![SENTINEL; 24];
        packed_rows_reference(&[1u8; 10], 50, p, &mut out);
        assert!(out.iter().all(|&v| v == SENTINEL));
        // Row 1 starts at byte 8 of 10: zero-extended; row 2 starts past the end.
        let mut out = vec![SENTINEL; 24];
        packed_rows(&[1u8; 10], 0, p, &mut out, false).unwrap();
        assert_eq!(&out[8..16], &[1, 1, 0, 0, 0, 0, 0, 0]);
        assert!(out[16..].iter().all(|&v| v == SENTINEL));
    }
}
