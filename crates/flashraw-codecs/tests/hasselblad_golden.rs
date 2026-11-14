//! Self-contained checks of the Hasselblad decoder: a plain reference decoder written from the
//! format description, properties (round trips, argument errors, truncation, parallel equals
//! sequential), and golden digests of decoded synthetic streams.

use flashraw_codecs::hasselblad as hb;

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

/// Code-length histograms of the two prefix codes found in real files (symbols as listed).
const TABLE_A: ([u8; 16], [u8; 17]) =
    ([0, 2, 1, 4, 2, 3, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0], [6, 7, 5, 4, 8, 9, 10, 3, 11, 1, 2, 12, 0, 13, 14, 15, 16]);
const TABLE_B: ([u8; 16], [u8; 17]) =
    ([0, 2, 2, 3, 1, 0, 3, 1, 1, 0, 4, 0, 0, 0, 0, 0], [7, 8, 6, 9, 4, 5, 10, 3, 1, 2, 11, 0, 12, 13, 14, 15, 16]);

/// (code, length) per symbol 0..=16, canonical assignment.
fn codes(t: &([u8; 16], [u8; 17])) -> Vec<(u32, u32)> {
    let mut out = vec![(0u32, 0u32); 17];
    let (mut code, mut k) = (0u32, 0usize);
    for len in 1..=16u32 {
        for _ in 0..t.0[len as usize - 1] {
            out[t.1[k] as usize] = (code, len);
            code += 1;
            k += 1;
        }
        code <<= 1;
    }
    out
}

struct Bits {
    words: Vec<u8>,
    acc: u64,
    n: u32,
}
impl Bits {
    fn put(&mut self, v: u32, b: u32) {
        if b == 0 {
            return;
        }
        self.acc = (self.acc << b) | (v as u64 & ((1u64 << b) - 1));
        self.n += b;
        while self.n >= 32 {
            self.words.extend(((self.acc >> (self.n - 32)) as u32).to_le_bytes());
            self.n -= 32;
        }
    }
    fn finish(mut self) -> Vec<u8> {
        if self.n > 0 {
            let b = 32 - self.n;
            self.put(0, b);
        }
        self.words
    }
}

/// A stream of `pairs` pixel pairs with random lengths (biased small) and random bits.
fn scan(r: &mut Rng, t: &([u8; 16], [u8; 17]), pairs: usize, big: bool) -> Vec<u8> {
    let c = codes(t);
    let mut b = Bits { words: vec![], acc: 0, n: 0 };
    for _ in 0..pairs {
        let mut l = [0u32; 2];
        for x in &mut l {
            *x = if big && r.below(50) == 0 {
                16
            } else {
                (r.below(9) as u32 + r.below(5) as u32).min(if big { 16 } else { 11 })
            };
            let (code, len) = c[*x as usize];
            b.put(code, len);
        }
        for &x in &l {
            let v = if x == 16 && r.below(2) == 0 { 0xFFFF } else { r.step() as u32 };
            b.put(v, x);
        }
    }
    b.finish()
}

fn header(w: usize, h: usize, t: &([u8; 16], [u8; 17]), pred: u8, extra_comment: bool) -> Vec<u8> {
    let mut o =
        vec![0xFF, 0xD8, 0xFF, 0xC3, 0, 11, 16, (h >> 8) as u8, h as u8, (w >> 8) as u8, w as u8, 1, 0, 0x11, 0];
    o.extend([0xFF, 0xC4, 0, 2 + 17 + 17, 0]);
    o.extend(t.0);
    o.extend(t.1);
    if extra_comment {
        o.extend([0xFF, 0xFE, 0, 6, b'a', b'b', b'c', b'd']);
    }
    o.extend([0xFF, 0xDA, 0, 8, 1, 0, 0, pred, 0, 0]);
    o
}

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
fn fnv(out: &[u16]) -> u64 {
    let mut h = FNV_OFFSET;
    for &v in out {
        for b in v.to_le_bytes() {
            h = (h ^ b as u64).wrapping_mul(0x0100_0000_01b3);
        }
    }
    h
}

fn kind<T>(r: &flashraw_core::Result<T>) -> &'static str {
    match r {
        Ok(_) => "ok",
        Err(e) => e.kind(),
    }
}

fn prefill(n: usize) -> Vec<u16> {
    (0..n).map(|i| 1000u16.wrapping_add((i % 251) as u16)).collect()
}

/// Independent reference: bit-by-bit reading, canonical codes searched linearly, the
/// prediction written as in the format description.
fn reference(d: &[u8], t: &([u8; 16], [u8; 17]), pred: u8, w: usize, h: usize, base: i32) -> Option<Vec<u16>> {
    let hdr_len = header(w, h, t, pred, false).len();
    let scan = &d[hdr_len..];
    let cs = codes(t);
    struct Rd<'a> {
        scan: &'a [u8],
        bit: usize,
    }
    impl Rd<'_> {
        fn bit(&mut self) -> Option<u32> {
            let word = self.bit / 32;
            let bytes = self.scan.get(word * 4..word * 4 + 4)?;
            let v = u32::from_le_bytes(bytes.try_into().unwrap());
            let r = (v >> (31 - self.bit % 32)) & 1;
            self.bit += 1;
            Some(r)
        }
        fn get(&mut self, n: u32) -> Option<u32> {
            let mut v = 0;
            for _ in 0..n {
                v = (v << 1) | self.bit()?;
            }
            Some(v)
        }
        fn sym(&mut self, cs: &[(u32, u32)]) -> Option<u32> {
            let (mut code, mut len) = (0u32, 0u32);
            loop {
                code = (code << 1) | self.bit()?;
                len += 1;
                if let Some(s) = (0..17).find(|&s| cs[s] == (code, len)) {
                    return Some(s as u32);
                }
                if len > 16 {
                    return None;
                }
            }
        }
    }
    let mut rd = Rd { scan, bit: 0 };
    let start = 0x8000i32.wrapping_add(base);
    let mut hist = vec![vec![0i32; w]; h];
    let mut out = vec![0u16; w * h];
    for r in 0..h {
        for c in (0..w).step_by(2) {
            let l = [rd.sym(&cs)?, rd.sym(&cs)?];
            for (k, &n) in l.iter().enumerate() {
                let v = rd.get(n)? as i64;
                let mut dv = if n == 0 {
                    0
                } else if v >> (n - 1) & 1 == 1 {
                    v
                } else {
                    v - ((1i64 << n) - 1)
                };
                if dv == 65535 {
                    dv = -32768;
                }
                let x = c + k;
                let p = if pred == 11 {
                    if x < 2 {
                        start
                    } else if r < 2 {
                        hist[r][x - 2]
                    } else {
                        hist[r][x - 2].wrapping_add(hist[r - 2][x] / 2).wrapping_sub(hist[r - 2][x - 2] / 2)
                    }
                } else if x < 2 {
                    start
                } else {
                    hist[r][x - 2]
                };
                hist[r][x] = p.wrapping_add(dv as i32);
                out[r * w + x] = hist[r][x] as u16;
            }
        }
    }
    Some(out)
}

#[test]
fn matches_reference_on_valid_streams() {
    let mut r = Rng(0x1234_5678_9abc_def1);
    for iter in 0..600 {
        let w = [2usize, 4, 6, 16, 34, 64, 100, 258][r.below(8)];
        let h = [1usize, 2, 3, 4, 5, 9, 20][r.below(7)];
        let t = if r.below(2) == 0 { &TABLE_A } else { &TABLE_B };
        let pred = [1u8, 8, 11, 11, 0, 5, 7][r.below(7)];
        let base = [0, 256, -1000, 70000, i32::MAX][r.below(5)];
        let mut d = header(w, h, t, pred, false);
        let big = r.below(4) == 0;
        d.extend(scan(&mut r, t, w * h / 2, big));
        d.extend([0xFF, 0xD9]);
        let hdr = hb::HasselbladHeader::parse(&d).unwrap();
        assert_eq!((hdr.predictor, hdr.width, hdr.height), (pred as u32, w as u32, h as u32));
        let want = reference(&d, t, pred, w, h, base).unwrap();
        let mut a = prefill(w * h);
        hb::decode(&d, &hdr, &mut a, w, h, base).unwrap();
        assert_eq!(a, want, "case {iter} {w}x{h} pred {pred} base {base}");
        let mut b = prefill(w * h);
        hb::decode_par(&d, &hdr, &mut b, w, h, base, true).unwrap();
        assert_eq!(a, b);
    }
}

#[test]
fn escape_value_is_minus_32768() {
    // One pair: lengths 16 and 1; first difference all ones (escape), second 1.
    let t = &TABLE_A;
    let c = codes(t);
    let mut b = Bits { words: vec![], acc: 0, n: 0 };
    b.put(c[16].0, c[16].1);
    b.put(c[1].0, c[1].1);
    b.put(0xFFFF, 16);
    b.put(1, 1);
    let mut d = header(2, 1, t, 8, false);
    d.extend(b.finish());
    let hdr = hb::HasselbladHeader::parse(&d).unwrap();
    let mut out = [0u16; 2];
    hb::decode(&d, &hdr, &mut out, 2, 1, 0).unwrap();
    assert_eq!(out, [0x8000u16.wrapping_sub(32768), 0x8001]);
}

#[test]
fn argument_errors_and_degenerate_sizes() {
    let mut r = Rng(99);
    let t = &TABLE_A;
    let mut d = header(8, 4, t, 8, false);
    d.extend(scan(&mut r, t, 16, false));
    let hdr = hb::HasselbladHeader::parse(&d).unwrap();
    for (w, h, outlen, want) in [
        (7usize, 4usize, 28usize, "UnsupportedCompression"),
        (8, 4, 31, "InvalidArgument"),
        (8, 4, 32, "ok"),
        (8, 4, 40, "ok"),
        (8, 0, 0, "ok"),
        (2, 1, 2, "ok"),
    ] {
        let mut o = prefill(outlen);
        assert_eq!(kind(&hb::decode(&d, &hdr, &mut o, w, h, 0)), want, "{w}x{h} out {outlen}");
        assert_eq!(kind(&hb::decode_par(&d, &hdr, &mut o, w, h, 0, true)), want);
    }
    let mut late = hdr.clone();
    late.scan_offset = d.len() + 5;
    let mut o = prefill(32);
    assert_eq!(kind(&hb::decode(&d, &late, &mut o, 8, 4, 0)), "CorruptFile");
    let mut z = prefill(4);
    hb::decode(&d, &hdr, &mut z, 0, 4, 0).unwrap();
    hb::decode_par(&d, &hdr, &mut z, 0, 4, 0, true).unwrap();
    assert_eq!(z, prefill(4));
    // a tail beyond width*height stays untouched
    let mut o = prefill(40);
    hb::decode(&d, &hdr, &mut o, 8, 4, 0).unwrap();
    assert_eq!(o[32..], prefill(40)[32..]);
}

#[test]
fn damaged_input_never_panics() {
    let mut r = Rng(0x0bad_cafe_0000_0007);
    for iter in 0..3000 {
        let (w, h) = ([2usize, 16, 66][r.below(3)], [1usize, 4, 12][r.below(3)]);
        let t = if r.below(2) == 0 { &TABLE_A } else { &TABLE_B };
        let mut d = header(w, h, t, if r.below(2) == 0 { 11 } else { 8 }, r.below(2) == 0);
        let so = d.len();
        d.extend(scan(&mut r, t, w * h / 2, true));
        for _ in 0..1 + r.below(3) {
            let i = if r.below(2) == 0 { r.below(so) } else { so + r.below(d.len() - so) };
            d[i] ^= 1 << r.below(8);
        }
        if r.below(4) == 0 {
            d.truncate(r.below(d.len() + 1));
        }
        let Ok(hdr) = hb::HasselbladHeader::parse(&d) else { continue };
        let mut a = prefill(w * h);
        let ra = hb::decode(&d, &hdr, &mut a, w, h, iter);
        let mut b = prefill(w * h);
        let rb = hb::decode_par(&d, &hdr, &mut b, w, h, iter, true);
        assert_eq!(kind(&ra), kind(&rb));
    }
    // every prefix of a header parses or fails cleanly; noise payloads
    let mut d = header(32, 8, &TABLE_A, 8, true);
    let n = d.len();
    for _ in 0..400 {
        d.push(r.step() as u8);
    }
    for cut in 0..=n {
        let _ = hb::HasselbladHeader::parse(&d[..cut]);
    }
    for pred in [11u8, 1] {
        let mut d = header(32, 8, &TABLE_A, pred, false);
        for _ in 0..400 {
            d.push(r.step() as u8);
        }
        let hdr = hb::HasselbladHeader::parse(&d).unwrap();
        let mut o = prefill(256);
        let _ = hb::decode(&d, &hdr, &mut o, 32, 8, 0);
    }
}

#[test]
fn truncation_inside_the_image_is_an_error() {
    let mut r = Rng(5);
    let (w, h) = (64usize, 30usize);
    let mut d = header(w, h, &TABLE_A, 8, false);
    let so = d.len();
    d.extend(scan(&mut r, &TABLE_A, w * h / 2, false));
    for cut in [so, so + 10, (so + d.len()) / 2] {
        let hdr = hb::HasselbladHeader::parse(&d[..cut]).unwrap();
        let mut o = prefill(w * h);
        assert_eq!(kind(&hb::decode(&d[..cut], &hdr, &mut o, w, h, 0)), "CorruptFile");
        assert_eq!(kind(&hb::decode_par(&d[..cut], &hdr, &mut o, w, h, 0, true)), "CorruptFile");
    }
    let hdr = hb::HasselbladHeader::parse(&d).unwrap();
    let mut o = prefill(w * h);
    hb::decode(&d, &hdr, &mut o, w, h, 0).unwrap();
}

/// Deterministic synthetic streams used by the golden digests and the parallel test.
fn large_cases() -> Vec<(String, Vec<u8>, usize, usize, i32)> {
    let mut r = Rng(0x5eed_5eed_5eed_5eed);
    let mut v = Vec::new();
    for (w, h, pred, big) in [(4096usize, 1100usize, 8u8, false), (4098, 1030, 1, true), (4096, 1100, 11, false)] {
        let mut d = header(w, h, &TABLE_A, pred, false);
        d.extend(scan(&mut r, &TABLE_A, w * h / 2, big));
        v.push((format!("large {w}x{h} pred {pred}"), d, w, h, 256));
    }
    v
}

#[test]
fn parallel_path_equals_sequential_on_large_images() {
    let mut r = Rng(77);
    for (name, d, w, h, base) in large_cases() {
        let hdr = hb::HasselbladHeader::parse(&d).unwrap();
        let so = hdr.scan_offset;
        let full = d.len();
        let mut flipped = d.clone();
        flipped[so + r.below(full - so)] ^= 0x40;
        let variants: Vec<(&str, &[u8])> = vec![
            ("full", &d[..]),
            ("cut5", &d[..full - 5]),
            ("cut17", &d[..full - 17]),
            ("half", &d[..so + (full - so) / 2]),
            ("flipped", &flipped[..]),
        ];
        for (vn, bytes) in variants {
            let mut a = prefill(w * h);
            let ra = hb::decode(bytes, &hdr, &mut a, w, h, base);
            let mut b = prefill(w * h);
            let rb = hb::decode_par(bytes, &hdr, &mut b, w, h, base, true);
            assert_eq!(kind(&ra), kind(&rb), "{name} {vn}");
            if ra.is_ok() {
                assert!(a == b, "{name} {vn}: output differs");
            }
        }
    }
}

/// Digests of the decoded output, taken from the pre-rewrite implementation.
const GOLDEN: &[(&str, u64)] = &[
    ("large 4096x1100 pred 8", 0xdf47_b640_cf22_3898),
    ("large 4098x1030 pred 1", 0xc581_4293_b4b7_1f75),
    ("large 4096x1100 pred 11", 0x9d38_4250_4c99_cd1d),
];

#[test]
fn golden_digests() {
    for (name, d, w, h, base) in large_cases() {
        let hdr = hb::HasselbladHeader::parse(&d).unwrap();
        let mut o = vec![0u16; w * h];
        hb::decode_par(&d, &hdr, &mut o, w, h, base, true).unwrap();
        let got = fnv(&o);
        let want = GOLDEN.iter().find(|g| g.0 == name).unwrap().1;
        assert_eq!(got, want, "{name}");
    }
}
