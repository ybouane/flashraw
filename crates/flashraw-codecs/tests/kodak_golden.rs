//! Self-contained tests of the Kodak codecs: encoder round trips for the 65000 coder, structural
//! properties, and golden digests of the other codecs on synthetic input (the digests were
//! produced by the pre-rewrite implementation and by this one; they agree).

use flashraw_codecs::kodak as k;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn fnv(h: &mut u64, bytes: &[u8]) {
    for &b in bytes {
        *h ^= u64::from(b);
        *h = h.wrapping_mul(0x100_0000_01b3);
    }
}
fn fnv16(h: &mut u64, v: &[u16]) {
    for x in v {
        fnv(h, &x.to_le_bytes());
    }
}

/// Encode one difference block of `vals` (12-bit samples) following the format description.
fn encode_diff_block(vals: &[u16]) -> Vec<u8> {
    let q = vals.len().div_ceil(4);
    let b = 4 * q;
    let mut pred = [0i32; 2];
    let mut lens = vec![0u32; b];
    let mut codes = vec![0u32; b];
    for i in 0..vals.len() {
        let d = i32::from(vals[i]) - pred[i & 1];
        pred[i & 1] = i32::from(vals[i]);
        if d == 0 {
            continue;
        }
        let l = 32 - d.unsigned_abs().leading_zeros();
        lens[i] = l;
        codes[i] = if d > 0 { d as u32 } else { (d + (1 << l) - 1) as u32 };
    }
    let mut out = vec![0u8; b / 2];
    for i in 0..b {
        out[i / 2] |= (lens[i] as u8) << (4 * (i & 1));
    }
    let sum: u32 = lens.iter().sum();
    let lead = if b % 8 == 4 { 16 } else { 0 };
    let refills = if sum <= lead { 0 } else { (sum - lead).div_ceil(32) };
    let words = (lead / 16 + 2 * refills) as usize;
    let mut bits = vec![0u16; words];
    let mut at = 0usize;
    for i in 0..b {
        for j in 0..lens[i] as usize {
            if codes[i] >> j & 1 == 1 {
                bits[at / 16] |= 1 << (at % 16);
            }
            at += 1;
        }
    }
    for w in bits {
        out.extend_from_slice(&w.to_be_bytes());
    }
    out
}

/// Encode a raw block: 12-bit samples, groups of eight, header nibbles replaced by the data.
fn encode_raw_block(vals: &[u16]) -> Vec<u8> {
    let groups = vals.len().div_ceil(8);
    let mut out = Vec::new();
    for g in 0..groups {
        let mut s = [0u16; 8];
        for (j, v) in s.iter_mut().enumerate() {
            *v = vals.get(8 * g + j).copied().unwrap_or(0);
        }
        let mut w = [0u16; 6];
        for j in 0..6 {
            w[j] = s[2 + j] & 0xFFF;
        }
        w[0] |= (s[0] >> 8) << 12;
        w[2] |= ((s[0] >> 4) & 15) << 12;
        w[4] |= (s[0] & 15) << 12;
        w[1] |= (s[1] >> 8) << 12;
        w[3] |= ((s[1] >> 4) & 15) << 12;
        w[5] |= (s[1] & 15) << 12;
        for x in w {
            out.extend_from_slice(&x.to_be_bytes());
        }
    }
    out
}

fn gen_row(r: &mut Rng, width: usize, with_raw: bool) -> (Vec<u16>, Vec<u8>) {
    let vals: Vec<u16> = (0..width)
        .map(|i| {
            let base = 2000 + (i as i32 * 7) % 500;
            let n = if r.below(8) == 0 { r.below(4096) as i32 } else { base + r.below(40) as i32 - 20 };
            n.clamp(0, 4095) as u16
        })
        .collect();
    let mut bytes = Vec::new();
    for blk in vals.chunks(256) {
        if with_raw && r.below(3) == 0 {
            // Raw blocks are recognised by a header nibble above 12: the data's own first
            // bytes must therefore contain one.
            let v = blk.to_vec();
            let enc = encode_raw_block(&v);
            if k::block_len_65000(&enc, 0, v.len()).map(|x| x.1) == Some(true) {
                bytes.extend(enc);
                continue;
            }
        }
        bytes.extend(encode_diff_block(blk));
    }
    (vals, bytes)
}

fn identity() -> Vec<u16> {
    (0..65536u32).map(|i| i as u16).collect()
}

#[test]
fn rows_65000_round_trip() {
    let mut r = Rng(0x1234_5678_9abc_def1);
    let id = identity();
    for width in [1usize, 2, 3, 4, 5, 7, 8, 9, 100, 255, 256, 257, 300, 512, 513, 700] {
        let rows = 5;
        let mut data = vec![0xAAu8; 3];
        let mut expect = Vec::new();
        for _ in 0..rows {
            let (v, b) = gen_row(&mut r, width, true);
            expect.push(v);
            data.extend(b);
        }
        let starts = k::scan_65000_rows(&data, 3, width, rows).unwrap();
        let mut pos = 3;
        for (i, want) in expect.iter().enumerate() {
            assert_eq!(starts[i], pos, "width {width} row {i}");
            let mut out = vec![0u16; width];
            let end = k::decode_65000_row(&data, pos, &mut out, &id).unwrap();
            // Raw-block rows decode to the absolute values; difference rows reproduce them too.
            assert_eq!(&out, want, "width {width} row {i}");
            let mut direct = vec![0u16; width];
            k::decode_65000_row(&data, starts[i], &mut direct, &id).unwrap();
            assert_eq!(direct, out);
            pos = end;
        }
        assert_eq!(pos, data.len());
    }
}

#[test]
fn block_functions_agree() {
    let mut r = Rng(77);
    for len in [1usize, 3, 4, 5, 8, 12, 64, 255, 256] {
        let vals: Vec<u16> = (0..len).map(|_| r.below(4096) as u16).collect();
        let b = encode_diff_block(&vals);
        let (n, raw) = k::block_len_65000(&b, 0, len).unwrap();
        assert_eq!((n, raw), (b.len(), false));
        let mut out = [0i16; 272];
        assert!(!k::decode_block_65000(&b, 0, len, &mut out).unwrap());
        let mut p = [0i32; 2];
        for i in 0..len {
            p[i & 1] += i32::from(out[i]);
            assert_eq!(p[i & 1], i32::from(vals[i]));
        }
        // Too small an output buffer and a cut header are reported.
        assert!(k::decode_block_65000(&b, 0, len, &mut out[..len.div_ceil(4) * 4 - 1]).is_err());
        assert!(k::block_len_65000(&b[..b.len().min(len.div_ceil(4) * 2 - 1)], 0, len).is_none());
        let raw = encode_raw_block(&vals);
        if k::block_len_65000(&raw, 0, len).map(|x| x.1) == Some(true) {
            let mut o = [0i16; 272];
            assert!(k::decode_block_65000(&raw, 0, len, &mut o).unwrap());
            for i in 0..len {
                assert_eq!(o[i] as u16, vals[i]);
            }
        }
    }
    // Hostile positions never panic.
    assert!(k::block_len_65000(&[0; 4], usize::MAX, 256).is_none());
    assert!(k::block_len_65000(&[], 0, usize::MAX).is_none());
    assert!(k::scan_65000_rows(&[], usize::MAX, 10, 3).is_err());
}

#[test]
fn dc120_rotation_properties() {
    let src: Vec<u8> = (0..848).map(|i| (i * 31 % 251) as u8).collect();
    let mut out = vec![0u16; 848];
    for row in [0usize, 1, 2, 3, 4, 977, 1 << 40] {
        k::dc120_row(&src, row, &mut out);
        let m = [162u128, 192, 187, 92][row % 4];
        let a = [0u128, 636, 424, 212][row % 4];
        let shift = ((row as u128 * m + a) % 848) as usize;
        for c in 0..848 {
            assert_eq!(out[c], u16::from(src[(c + shift) % 848]));
        }
    }
    k::dc120_row(&[], 3, &mut out);
    k::dc120_row(&src, usize::MAX, &mut out[..5]);
}

#[test]
fn ycc_rows_properties() {
    let curve = identity();
    // Grey: Y with neutral chroma gives R = G = B = Y.
    let mut px = vec![0u8; 2 * 16];
    for (i, p) in px.iter_mut().enumerate() {
        *p = if i % 2 == 0 { 100 + i as u8 } else { 128 };
    }
    let mut out = vec![0u16; 3 * 8];
    k::c330_row(&px, 8, &curve, &mut out);
    for c in 0..8 {
        assert_eq!(&out[3 * c..3 * c + 3], &[px[2 * c].into(); 3]);
    }
    let mut rec = vec![128u8; 3 * 8];
    rec[..8].fill(50);
    rec[16..].fill(200);
    k::c603_row(&rec, 8, false, &curve, &mut out);
    assert!(out.iter().all(|&v| v == 50));
    k::c603_row(&rec, 8, true, &curve, &mut out);
    assert!(out.iter().all(|&v| v == 200));
    // Short inputs never panic.
    k::c330_row(&[], 8, &[], &mut out);
    k::c603_row(&[1, 2], 1 << 40, true, &curve[..10], &mut out);
}

#[test]
fn gamma_curve_shape() {
    let c = k::gamma_curve(0.0, 3.875, 255);
    assert_eq!(c.len(), 65536);
    assert_eq!(c[0], 0);
    assert!(c[255..].iter().all(|&v| v == 65535));
    assert!(c.windows(2).all(|w| w[0] <= w[1]));
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    fnv16(&mut h, &c);
    assert_eq!(h, GOLDEN_GAMMA, "digest {h:#x}");
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for (p, t, m) in [(0.45, 4.5, 255u32), (1.0, 2.0, 1000), (2.4, 12.92, 65535), (0.0, 0.0, 0), (-1.0, 0.5, 4095)] {
        fnv16(&mut h, &k::gamma_curve(p, t, m));
    }
    assert_eq!(h, GOLDEN_GAMMA_GRID, "digest {h:#x}");
}

fn radc_digest(seed: u64) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    let mut r = Rng(seed);
    for case in 0..24 {
        let width = [4usize, 8, 16, 64, 128, 768][case % 6];
        let height = [4usize, 8, 16, 32][case % 4];
        let cbpp = [0u32, 243, 152][case % 3];
        let bytes: Vec<u8> = (0..width * height * 4).map(|_| (r.next() >> 24) as u8).collect();
        let mut out = vec![0u16; width * height];
        match k::decode_radc(&bytes, width, height, cbpp, &mut out) {
            Ok(()) => {
                fnv(&mut h, &[1]);
                fnv16(&mut h, &out);
            }
            Err(e) => {
                fnv(&mut h, &[0]);
                assert_eq!(e.kind(), "CorruptFile");
            }
        }
    }
    h
}

#[test]
fn radc_synthetic_digest() {
    assert_eq!(radc_digest(0x9e37_79b9_7f4a_7c15), GOLDEN_RADC, "digest {:#x}", radc_digest(0x9e37_79b9_7f4a_7c15));
}

#[test]
fn radc_geometry_and_truncation() {
    let mut out = vec![0u16; 768 * 512];
    for (w, h) in [(0usize, 4usize), (3, 4), (769, 4), (8, 5), (8, 516)] {
        assert!(k::decode_radc(&[0xFF; 4000], w, h, 0, &mut out).is_err(), "{w}x{h}");
    }
    assert!(k::decode_radc(&[], 768, 512, 0, &mut out).is_err());
    assert!(k::decode_radc(&[0xFF; 100], 768, 512, 0, &mut out[..100]).is_err());
    assert!(k::decode_radc(&[], 8, 0, 0, &mut out).is_ok());
    // Widths that are not multiples of four must not panic.
    let junk: Vec<u8> = (0..20000u32).map(|i| (i.wrapping_mul(2654435761u32) >> 13) as u8).collect();
    for w in 4..40 {
        let _ = k::decode_radc(&junk, w, 8, 0, &mut out);
    }
}

#[test]
fn radc_real_file() {
    let Ok(base) = std::env::var("FLASHRAW_CORPUS") else { return };
    let Ok(file) = std::fs::read(std::path::Path::new(&base).join("Kodak/DC50/RAW_KODAK_DC50.KDC")) else { return };
    let mut out = vec![0u16; 768 * 512];
    k::decode_radc(&file[19712..], 768, 512, 0, &mut out).unwrap();
    assert!(out.iter().all(|&v| v <= 16383));
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    fnv16(&mut h, &out);
    assert_eq!(h, GOLDEN_DC50, "digest {h:#x}");
    // Truncated: an error, never a panic.
    let mut o2 = vec![0u16; 768 * 512];
    assert!(k::decode_radc(&file[19712..19712 + 5000], 768, 512, 0, &mut o2).is_err());
}

const GOLDEN_GAMMA: u64 = 0x9b9c348679f1778e;
const GOLDEN_GAMMA_GRID: u64 = 0x9e516835fecad3ac;
const GOLDEN_RADC: u64 = 0xe4373543c2b3805e;
const GOLDEN_DC50: u64 = 0xc4a2220c9cbd8cb9;
