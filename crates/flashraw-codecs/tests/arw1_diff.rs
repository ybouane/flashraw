//! Tests of the ARW1 decoder against a reference encoder: random valid streams (small and large,
//! with 15/16/17-bit differences and the -32768 symbol) must decode to the image they were made
//! from (and, for the small ones, to a result pinned by a hash computed once against the
//! pre-rewrite decoder), truncated streams must fail, garbage and bad arguments must not panic.

use flashraw_codecs::sony_arw1 as new;

fn fnv(h: &mut u64, v: &[u16]) {
    for x in v {
        for b in x.to_le_bytes() {
            *h = (*h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3);
        }
    }
}

/// The image a stream of `targets` (running sums in stream order) describes, `stored` rows, with
/// `fill` in rows the stream does not visit. Odd-height streams have no odd rows (zeros).
fn expected(targets: &[i32], width: usize, coded: usize, stored: usize) -> Vec<u16> {
    let even = coded.div_ceil(2);
    let per_col = if coded % 2 == 0 { coded } else { even };
    let mut img = vec![0u16; width * stored.min(coded)];
    for row in 0..stored.min(coded) {
        let rank = if row % 2 == 0 {
            row / 2
        } else if coded % 2 == 0 {
            even + row / 2
        } else {
            continue;
        };
        for c in 0..width {
            img[row * width + c] = targets[(width - 1 - c) * per_col + rank] as u16;
        }
    }
    img
}

struct Rng(u64);
impl Rng {
    fn step(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.step() % n as u64) as usize
    }
}

/// Prefix code table of the format as (code length, diff length), in code-assignment order.
const CODE: [(u32, u32); 18] = [
    (15, 17),
    (15, 16),
    (14, 15),
    (13, 14),
    (12, 13),
    (11, 12),
    (10, 11),
    (9, 10),
    (8, 9),
    (7, 8),
    (6, 7),
    (5, 6),
    (4, 5),
    (3, 4),
    (3, 3),
    (3, 0),
    (2, 2),
    (2, 1),
];

struct BitWriter {
    out: Vec<u8>,
    acc: u64,
    n: u32,
}
impl BitWriter {
    fn put(&mut self, v: u32, len: u32) {
        for i in (0..len).rev() {
            self.acc = self.acc << 1 | ((v >> i) & 1) as u64;
            self.n += 1;
            if self.n == 8 {
                self.out.push(self.acc as u8);
                self.acc = 0;
                self.n = 0;
            }
        }
    }
}

/// Encode a sequence of running-sum targets in stream order (columns right to left, even rows
/// then odd rows). `values[col][k]` is the k-th visited sample of column `col`.
fn encode(targets: &[i32], pad_bytes: usize) -> Vec<u8> {
    let mut starts = vec![];
    let mut s = 0u32;
    for &(cl, _) in &CODE {
        starts.push(s >> (15 - cl)); // code value of this entry
        s += 32768 >> cl;
    }
    let mut w = BitWriter { out: vec![], acc: 0, n: 0 };
    let mut sum = 0i32;
    for &v in targets {
        let d = v.wrapping_sub(sum);
        sum = v;
        let (idx, bits, nbits) = if d == -32768 {
            (1, 0, 0)
        } else {
            let n = if d == 0 { 0 } else { 32 - d.unsigned_abs().leading_zeros() };
            let b = if d < 0 { (d - 1) as u32 & ((1u32 << n) - 1) } else { d as u32 };
            (CODE.iter().position(|c| c.1 == n).unwrap(), b, n)
        };
        w.put(starts[idx], CODE[idx].0);
        w.put(bits, nbits);
    }
    w.put(0, 7);
    w.out.extend(std::iter::repeat_n(0, pad_bytes));
    w.out
}

/// Random smooth-ish 12-bit image targets in stream order for a width x coded_rows image.
fn targets(r: &mut Rng, width: usize, coded: usize, with_excursions: bool) -> Vec<i32> {
    let mut v = Vec::with_capacity(width * coded);
    let mut cur = 2048i32;
    let jumpy = r.below(3);
    for _ in 0..width * coded {
        let step = match jumpy {
            0 => r.below(9) as i32 - 4,
            1 => r.below(200) as i32 - 100,
            _ => r.below(4096) as i32 - cur,
        };
        cur = (cur + step).clamp(0, 4095);
        v.push(cur);
    }
    // Rare wide excursions (diff lengths 15, 16 and 17 and the 16 -> -32768 symbol), each back in
    // range within a few samples. Kept to a handful so a real file would still count as sane.
    if with_excursions && v.len() > 8 {
        for _ in 0..3 {
            let i = 1 + r.below(v.len() - 4);
            let prev = v[i - 1];
            match r.below(3) {
                0 => {
                    v[i] = prev - 32768; // exactly the -32768 symbol
                    v[i + 1] = prev - 16384;
                    v[i + 2] = prev;
                }
                1 => {
                    v[i] = prev + 65536; // 17-bit difference up, 17-bit difference down
                    v[i + 1] = prev;
                }
                _ => {
                    v[i] = prev - 16384;
                    v[i + 1] = prev;
                }
            }
        }
    }
    v
}

#[test]
fn valid_streams_match() {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    let mut r = Rng(0x1234_5678_9abc_def1);
    for case in 0..400 {
        let width = 1 + r.below(70);
        let coded = 1 + r.below(40);
        let stored = if r.below(2) == 0 { coded } else { r.below(coded + 1) };
        let t = targets(&mut r, width, coded, false);
        let src = encode(&t, 16);
        let fill = 0xA5A5u16;
        let mut b = vec![fill; width * stored];
        new::decode(&src, &mut b, width, coded, stored).unwrap();
        let want = expected(&t, width, coded, stored);
        assert_eq!(b[..want.len()], want[..], "case {case} {width}x{coded}/{stored}");
        assert!(b[want.len()..].iter().all(|&v| v == fill), "case {case}: rows past the stream are untouched");
        fnv(&mut hash, &b);
        for par in [false, true] {
            let mut c = vec![fill; width * stored];
            new::decode_par(&src, &mut c, width, coded, stored, par).unwrap();
            assert_eq!(b, c, "case {case} par {par}");
        }
    }
    assert_eq!(hash, 0x9049_dd6d_ffb8_2967);
}

#[test]
fn large_streams_match_parallel() {
    let mut r = Rng(42);
    // The last two exceed the size from which the speculative parallel route is used.
    for (width, coded, stored) in
        [(700usize, 500usize, 500usize), (2400, 2000, 2000), (2301, 2002, 1990), (5000, 1000, 1000)]
    {
        let t = targets(&mut r, width, coded, true);
        let src = encode(&t, 100);
        let want = expected(&t, width, coded, stored);
        let mut seq = vec![0u16; width * stored];
        new::decode(&src, &mut seq, width, coded, stored).unwrap();
        assert_eq!(seq, want);
        let mut par = vec![0u16; width * stored];
        new::decode_par(&src, &mut par, width, coded, stored, true).unwrap();
        assert_eq!(par, want);
        let cut = src.len() * 2 / 3;
        assert!(new::decode_par(&src[..cut], &mut par, width, coded, stored, true).is_err());
        assert!(new::decode(&src[..cut], &mut seq, width, coded, stored).is_err());
    }
}

#[test]
fn truncated_streams_fail() {
    let mut r = Rng(7);
    for _ in 0..60 {
        let (width, coded) = (8 + r.below(50), 2 * (4 + r.below(20)));
        let t = targets(&mut r, width, coded, false);
        let src = encode(&t, 0);
        let cut = src.len() / 4 + r.below(src.len() / 2);
        let mut b = vec![0u16; width * coded];
        assert!(new::decode(&src[..cut], &mut b, width, coded, coded).is_err());
    }
}

#[test]
fn garbage_and_bad_arguments_do_not_panic() {
    let mut r = Rng(99);
    for _ in 0..300 {
        let n = r.below(600);
        let src: Vec<u8> = (0..n).map(|_| r.step() as u8).collect();
        let (width, coded, stored) = (r.below(40), r.below(30), r.below(30));
        let mut out = vec![0u16; r.below(width * stored + 5)];
        let _ = new::decode(&src, &mut out, width, coded, stored);
        let _ = new::decode_par(&src, &mut out, width, coded, stored, true);
    }
}

#[test]
fn absurd_geometry_is_rejected_cheaply() {
    let mut out = vec![0u16; 8];
    assert!(new::decode(&[1, 2, 3], &mut out, 2, 1 << 40, 2).is_err());
    assert!(new::decode(&[1, 2, 3], &mut out, 2, usize::MAX, 2).is_err());
    assert!(new::decode(&[1, 2, 3], &mut out, usize::MAX, 4, 4).is_err());
    assert!(new::decode(&[], &mut out, 2, 4, 4).is_err());
    assert!(new::decode(&[], &mut out, 0, 4, 4).is_ok());
    assert!(new::decode(&[], &mut out, 2, 0, 4).is_ok());
    assert!(new::decode(&[], &mut out, 2, 4, 0).is_ok());
}
