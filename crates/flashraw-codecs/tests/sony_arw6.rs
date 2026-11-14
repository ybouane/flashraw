//! ARW6 decoder tests on synthetic strips (the pinned real files live in the regression suite).
//!
//! (Eleven golden cases changed when the coefficient width bound moved from 19 to 31 bits: they are
//! bit-flipped streams whose row used to be abandoned at a width of 20 to 31.)
//! The golden hashes were produced by the implementation and checked against an independent
//! earlier implementation over hundreds of thousands of strips.

mod sony_arw6_gen;

use flashraw_codecs::sony_arw6::{decode, find_streams};
use flashraw_codecs::sony_arw6_curve::LLVC3_CURVE;
use sony_arw6_gen::*;

const GOLDEN_CASES: u64 = 240;

fn fnv(bytes: impl Iterator<Item = u8>, mut h: u64) -> u64 {
    for b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Hash of everything observable about decoding a case: the streams found, the result
/// kind and the whole output buffer (sentinel-filled before decoding).
fn digest(c: &Case, parallel: bool) -> u64 {
    let mut h = 0xcbf29ce484222325u64;
    for t in find_streams(&c.strip, c.raw_w, c.raw_h) {
        let v = [t.offset, t.length, t.x, t.y, t.width, t.height];
        h = fnv(v.iter().flat_map(|x| (*x as u64).to_le_bytes()), h);
    }
    let mut out = vec![0xA5A5u16; c.raw_w * c.raw_h];
    let r = decode(&c.strip, &mut out, c.raw_w, c.raw_h, parallel);
    match r {
        Ok(()) => {
            h = fnv(out.iter().flat_map(|v| v.to_le_bytes()), h);
        }
        Err(e) => {
            h = fnv(e.kind().bytes(), h);
        }
    }
    h
}

#[rustfmt::skip]
const GOLDEN: &[u64] = &[
0xef838829379812d4, 0x81b449e8ada20481, 0xb867bd734d9a93bd, 0x08e80da4df7f79b0,
0xef838829379812d4, 0xe2554e3c2e168f69, 0x7512900630fa0ef0, 0xef838829379812d4,
0xe70f4b6a364c8098, 0xa657a064c9d3c902, 0xd450fa059c1f2718, 0x9d90c0eae9c79230,
0x484a39af29ac23dc, 0xed4315ef7a9bc45b, 0x586a1e9250bd3147, 0x51dddc983f7dd52c,
0xef838829379812d4, 0xf8dcc9dd0e826e1e, 0x11d0ed0c875dd09f, 0xef838829379812d4,
0x5c25e7c80928e6b8, 0xf824d19a47e96e92, 0x7ee049a918b1ba6e, 0x3af87e4bd3b20dd1,
0x6b41078bf88c354a, 0xdb033ccc9a1b52cb, 0x69fcd60ce24d75b1, 0x3b3b22da5309fd52,
0x91268521fb673ac5, 0x9b65c7453d59eab8, 0x4b1eb131fbf4c97d, 0x773a0b3535d24c75,
0x3803b4c87f27192f, 0x21914ddf75be1197, 0x0f728293cdd11fdf, 0x719873c107bfbe0c,
0xef838829379812d4, 0x163149496b8f97b5, 0xef838829379812d4, 0xb4e95f067f5fd0a3,
0x11d459590f515c01, 0x55c0c5f2badf2d5a, 0xef838829379812d4, 0xef838829379812d4,
0xb34f09c9be7b4a4a, 0x6d89fdd20447651c, 0x220c86d671802ee6, 0xc7a1b94a33eb49d0,
0xef838829379812d4, 0x96fac5d7baa98636, 0x1a1351a5b9f73515, 0x98bcf1a9a653e0b6,
0x6ad430f82d734d2b, 0x1e3ae057e0998d1b, 0x74f40265d5b7d3d9, 0xd41673fd95f54c5b,
0x1bb0c5b88a36740c, 0xba4bd8f89af80554, 0x1e04c301a03a0b0a, 0x490f86da526cda29,
0xb1cdfda658d3d6b4, 0xef838829379812d4, 0x7cb20bd2e80f48fb, 0xb0704aedf8d7c9f1,
0x0bd6dccd370ef37f, 0xef838829379812d4, 0x98c00264d537e197, 0xd2846a21a60aba10,
0x4486f8998da93fd3, 0xdf1f17787a76834f, 0xf7054254a52bd1f1, 0x9a6be1ffaa832597,
0xa45c2136e3dffd0e, 0xf1bad626b4ff8e87, 0x43272b593e44cf76, 0x8ff43649780bf433,
0x85e7ebc23072a528, 0x576186255be5e8c8, 0x04855833e4adc4b6, 0xeece7ccc7c842f49,
0x698b3eea8ab0d54d, 0x84b0640a499841c5, 0xa5c19ce34bde089f, 0xf3288bf266f84eb0,
0xe79d57a5e4e69737, 0xa60d01c06acd1479, 0x27ceaa18718eee55, 0x5e8a8834ccb74f59,
0x9e2572481bc3f3da, 0x501c610ba8d27661, 0xe029c6cf84db4f85, 0x56d18837c7f45349,
0xe5bbbf058edb556e, 0x9eae4a05aa3898da, 0x6a83ebf935992cce, 0x9551fc755f5110f1,
0xd1e29ccc20539bcd, 0x7b07014f48a9f563, 0xc7e59382d040183c, 0x79eaaad712871200,
0xd224997a0fa9794e, 0xef838829379812d4, 0xe8c25c5fd71f4c70, 0xc35e1a7dc62d9a6c,
0x9fe8293659eeabe3, 0xea8f5e7454f719a3, 0xa103a8c1be7f379e, 0xf305b18e702d17bf,
0x1217dafebb2575c6, 0x817400ff1fecf0d0, 0x0eda6dcb0c6a697a, 0x7845cd77c0eae71d,
0xf95c3cdc400f12d6, 0xc5ebb66a7fed1b43, 0x9fa360347754464f, 0x3d24101709d33ea2,
0x74bb5f1601e92a6f, 0xd9095f2ec2263ee4, 0xef838829379812d4, 0x5da037f9c6bd5650,
0x024f422905d83d1b, 0x3fef8c3f06dbf6d9, 0xc0cc34f59597566d, 0xf9ea04440d945ac6,
0x97a486bbdef59b5a, 0xf7aac47e44009dd1, 0x656c63a2fb6128c3, 0x11dfc594cecb3bba,
0x20be493990668434, 0xef838829379812d4, 0x147a88c1e6fc080f, 0xbd2108c4d775240e,
0xc0ce22ac54330003, 0x1a1e8f88d80a680f, 0xa02fa861f6bb4057, 0xef838829379812d4,
0x2c20b47c503d421f, 0x70ef97aac0a5a0c0, 0x721076cfe0c495b3, 0xef838829379812d4,
0x3117a3c4d529c914, 0x0a98fd1ee9498448, 0x7131d83553722c35, 0x7b3c6cf28c73f35c,
0x921bd991a3222956, 0xf45024e8feb98d4a, 0x2f2391584afd6450, 0xef838829379812d4,
0x5a080b55f9adce26, 0x055827ae532893ba, 0xef838829379812d4, 0x80aa94f46618bb76,
0x407092dfd7af1faf, 0xbc8702784f8baf6e, 0x53e65281024aaa9a, 0x2bbacf2ffa85f032,
0x20ab76dda592fec7, 0x044b5ceb2e1ef5b5, 0x5312ac91a38bfb3e, 0x8b11136e73b28092,
0x177dea25d7e2cdc5, 0x8de1bd7b3fd6ce24, 0x8d62500909342142, 0x9fd8bfe73a0b95c6,
0xef838829379812d4, 0x1a9663b6d8ab40c7, 0xf6952ec7546bbd00, 0x07e73f3bad33ed76,
0xd8c0bef7805955fd, 0xa3a43ef507f7aa07, 0x66efaf51602735e9, 0x9ac60037491b7870,
0x3e8f79d0203eb4dd, 0xe3c171c9f8ecfc6e, 0xef838829379812d4, 0x02d6e7518f9d78e2,
0xc6b0dba63d1160ed, 0x5669b32f496fb967, 0x24f57d48ca5ddd53, 0x6f5f66786fb02623,
0xef838829379812d4, 0xf802baff96851d26, 0xef838829379812d4, 0xdc35dfd7a2079b18,
0xc54399953b4b222c, 0xe632830a6931c77a, 0xcb3a8a3a857be205, 0x46a39be2580dea5a,
0xc4804e1a883bc215, 0xa69b9ac62031a9a3, 0xef838829379812d4, 0xc8aed14a89b8c077,
0x793f46c0bf7723b3, 0x2981a7703c6c9f83, 0x97d4a4a87c437cbb, 0xfeb674f64a6a7fd3,
0xe8ab57edba188334, 0x1e1d9d06ad925a24, 0x742b729a977cc412, 0xef838829379812d4,
0xd3b8d081eca00bbb, 0x31cd9cf7a899bc5f, 0x77d7abc940b0d6dd, 0xa548763d708da820,
0x9adef98706b3a1f7, 0xf2268031148ac115, 0x2b710ea1cd4d0afe, 0x1fda0b818d582d54,
0xa4ec7606071cfed4, 0xdee6246d8bf89c58, 0x1f41385e46f26783, 0x7ca8e11caa157048,
0x723ab3d5815d0561, 0x6eb88aa6d25d09ba, 0xef838829379812d4, 0xa62bec1f4916b5c2,
0xab0d7ca48ce0fb38, 0xcbac77606f559f08, 0xa3c4a4483dbb4cf1, 0xef838829379812d4,
0x4dd091c95b9d6657, 0x4c0a60baec3b939a, 0x0e59c15ec30c4ab8, 0xe53a8df4fbf0c805,
0x2b3fb04ac9a56463, 0xef838829379812d4, 0x523af506fedbedbd, 0xef838829379812d4,
0x6a4c05a3e88809f4, 0xc666dd5a66748a14, 0x39864e8465024b92, 0xcc5e9ddc0f2ccbf8,
0x33df731fc9fd04e1, 0x805921123aeb706b, 0xef838829379812d4, 0x551c4f609b0def70,
0xe757c13247b7f561, 0xf8935bda9e73e205, 0xba3a3a1253700ee1, 0xb503ed8903e923fe,
];

#[test]
fn curve_is_the_format_table() {
    assert_eq!(fnv(LLVC3_CURVE.iter().flat_map(|v| v.to_le_bytes()), 0xcbf29ce484222325), 0xf1f76888a4ec5e13);
    assert!(LLVC3_CURVE.windows(2).all(|p| p[0] <= p[1]));
    assert!(LLVC3_CURVE[..1428].iter().enumerate().all(|(i, &v)| v as usize == i));
    assert_eq!(LLVC3_CURVE[4095], 39002);
}

#[test]
fn synthetic_strips_match_golden_hashes() {
    if std::env::var("ARW6_PRINT_GOLDEN").is_ok() {
        for s in 0..GOLDEN_CASES {
            println!("{:#018x},", digest(&case(s), false));
        }
        return;
    }
    for s in 0..GOLDEN_CASES {
        let c = case(s);
        assert_eq!(digest(&c, false), GOLDEN[s as usize], "case {s}");
        assert_eq!(digest(&c, true), GOLDEN[s as usize], "case {s} (parallel)");
    }
}

/// Every decoded sample is a curve value, and nothing outside the tiles is written.
#[test]
fn output_values_and_untouched_areas() {
    let mut decoded = 0;
    for s in 0..600 {
        let c = case(1000 + s);
        let mut out = vec![0xA5A5u16; c.raw_w * c.raw_h];
        if decode(&c.strip, &mut out, c.raw_w, c.raw_h, true).is_err() {
            continue;
        }
        decoded += 1;
        let tiles = find_streams(&c.strip, c.raw_w, c.raw_h);
        for y in 0..c.raw_h {
            for x in 0..c.raw_w {
                let v = out[y * c.raw_w + x];
                let inside = tiles.iter().any(|t| x >= t.x && x < t.x + t.width && y >= t.y && y < t.y + t.height);
                if inside {
                    assert!(
                        LLVC3_CURVE.binary_search(&v).is_ok(),
                        "case {s}: sample {v} at ({x},{y}) is not a curve value"
                    );
                } else {
                    assert_eq!(v, 0xA5A5, "case {s}: ({x},{y}) outside every tile was written");
                }
            }
        }
    }
    assert!(decoded > 40, "the generator should decode often enough ({decoded})");
}

/// A stream whose records are all empty has all-zero coefficients: every plane stays zero
/// through the synthesis and the mosaic is the curve at code 0.
#[test]
fn empty_records_decode_to_mid_code() {
    let o = Options { extra_packets: 0, flaw: 0, fill: Fill::Uniform, max_record: 0, wide_selectors: false };
    let mut ok = 0;
    for (w, hh) in [(16usize, 16usize), (48, 24), (64, 9), (32, 10), (80, 12), (96, 11), (16, 8), (128, 17)] {
        let mut rng = Rng(w as u64 * 131 + hh as u64);
        let stream = stream(&mut rng, w, hh, &o);
        let mut strip = vec![0u8; 0x200];
        strip.extend(&stream);
        strip.extend([0u8; 0x40]);
        let h = 2 * hh;
        let mut out = vec![1u16; w * h];
        match decode(&strip, &mut out, w, h, false) {
            Ok(()) => {
                ok += 1;
                assert!(out.iter().all(|&v| v == LLVC3_CURVE[2048]), "{w}x{h}");
            }
            // Coded heights 10, 12 and 14 modulo 16 leave too few rows by design.
            Err(e) => assert_eq!(e.kind(), "CorruptFile"),
        }
    }
    assert!(ok >= 5);
}

#[test]
fn never_panics_on_damaged_strips() {
    for s in 0..400u64 {
        let c = case(5000 + s);
        let mut rng = Rng(s);
        for _ in 0..4 {
            let mut strip = c.strip.clone();
            for _ in 0..1 + rng.below(6) {
                let region = if rng.chance(70) { strip.len().min(0x300) } else { strip.len() };
                let at = rng.below(region);
                strip[at] = rng.next() as u8;
            }
            if rng.chance(40) {
                let cut = rng.below(strip.len() + 1);
                strip.truncate(cut);
            }
            let streams = find_streams(&strip, c.raw_w, c.raw_h);
            for t in &streams {
                assert!(t.offset + t.length <= strip.len());
            }
            let mut a = vec![0u16; c.raw_w * c.raw_h];
            let mut b = a.clone();
            let ra = decode(&strip, &mut a, c.raw_w, c.raw_h, false);
            let rb = decode(&strip, &mut b, c.raw_w, c.raw_h, true);
            assert_eq!(ra.is_ok(), rb.is_ok());
            if ra.is_ok() {
                assert!(a == b);
            }
        }
    }
}

#[test]
fn tiny_and_empty_inputs_are_errors() {
    let mut out = vec![0u16; 64 * 64];
    for len in [0usize, 1, 0x200, 0x27f, 0x280] {
        let strip = vec![0u8; len];
        assert!(find_streams(&strip, 64, 64).is_empty());
        assert!(decode(&strip, &mut out, 64, 64, false).is_err());
    }
    assert!(decode(&[0u8; 0x300], &mut out, 0, 64, false).is_err());
    assert!(decode(&[0u8; 0x300], &mut out, 60, 64, false).is_err());
    assert!(decode(&[0u8; 0x300], &mut out, 64, 63, false).is_err());
    assert!(decode(&[0u8; 0x300], &mut out[..100], 64, 64, false).is_err());
}

/// A stream declaring an enormous tile whose packets are missing is rejected up front, without
/// allocating the planes (the header and directory alone are enough).
#[test]
fn oversized_planes_are_rejected_quickly() {
    let (w, hh) = (65520usize, 2050usize);
    let mut strip = vec![0u8; 0x280];
    strip[0x200..0x204].copy_from_slice(b"A000");
    strip[0x208..0x20a].copy_from_slice(&(w as u16).to_be_bytes());
    strip[0x20a..0x20c].copy_from_slice(&(hh as u16).to_be_bytes());
    strip[0x20c] = 0x3d;
    strip[0x20e] = 0x6e;
    let h = 2 * hh;
    assert_eq!(find_streams(&strip, w, h).len(), 1);
    let mut out = vec![0u16; w * h];
    let t = std::time::Instant::now();
    let e = decode(&strip, &mut out, w, h, true).unwrap_err();
    assert_eq!(e.kind(), "CorruptFile");
    assert!(e.to_string().contains("missing packet"), "{e}");
    assert!(t.elapsed().as_secs() < 5);
}
