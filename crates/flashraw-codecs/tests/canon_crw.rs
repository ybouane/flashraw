//! Tests of the CRW decoder: an independent encoder (written from the format description) produces
//! valid streams for many geometries, which must decode back exactly; plus golden hashes, truncation,
//! malformed-input and argument checks.

use flashraw_codecs::canon_crw as new;

const FIRST: [(&[u8; 16], &[u8]); 3] = [
    (&[0, 1, 4, 2, 3, 1, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0], &[4, 3, 5, 6, 2, 7, 1, 8, 9, 0, 10, 11, 255]),
    (&[0, 2, 2, 3, 1, 1, 1, 1, 2, 0, 0, 0, 0, 0, 0, 0], &[3, 2, 4, 1, 5, 0, 6, 7, 9, 8, 10, 11, 255]),
    (&[0, 0, 6, 3, 1, 1, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0], &[6, 5, 7, 4, 8, 3, 9, 2, 0, 10, 1, 11, 255]),
];
const SECOND: [(&[u8; 16], &[u8]); 3] = [
    (
        &[0, 2, 2, 2, 1, 4, 2, 1, 2, 5, 1, 1, 0, 0, 0, 139],
        &[
            3, 4, 2, 5, 1, 6, 7, 8, 18, 19, 17, 20, 9, 21, 34, 0, 33, 22, 10, 240, 35, 23, 36, 49, 50, 24, 25, 51, 37,
            65, 52, 66, 53, 81, 54, 55, 56, 41, 121, 38, 26, 57, 86, 87, 40, 39, 82, 85, 88, 67, 118, 89, 119, 84, 97,
            249, 113, 120, 117, 150, 151, 73, 183, 83, 215, 116, 182, 152, 71, 72, 149, 105, 153, 145, 250, 184, 104,
            181, 185, 214, 247, 216, 103, 70, 69, 148, 137, 248, 129, 213, 246, 180, 136, 177, 42, 68, 114, 217, 135,
            102, 212, 245, 58, 167, 115, 169, 168, 134, 98, 199, 101, 200, 201, 161, 244, 209, 233, 90, 146, 133, 166,
            231, 147, 232, 193, 198, 122, 100, 225, 74, 106, 230, 179, 241, 211, 165, 138, 178, 154, 186, 132, 164, 99,
            229, 197, 243, 210, 196, 130, 170, 218, 228, 242, 202, 131, 163, 162, 195, 234, 194, 226, 227,
        ],
    ),
    (
        &[0, 2, 2, 1, 4, 1, 4, 1, 3, 3, 1, 0, 0, 0, 0, 140],
        &[
            2, 3, 1, 4, 5, 18, 17, 6, 19, 7, 8, 20, 34, 9, 33, 0, 35, 21, 49, 50, 10, 22, 240, 36, 51, 65, 66, 25, 23,
            37, 24, 81, 52, 67, 82, 41, 53, 97, 57, 113, 98, 54, 83, 38, 56, 26, 55, 129, 39, 145, 121, 85, 69, 40,
            114, 89, 161, 177, 68, 105, 84, 88, 209, 250, 87, 225, 241, 185, 73, 71, 99, 106, 249, 86, 70, 168, 42, 74,
            120, 153, 58, 117, 116, 134, 101, 193, 118, 182, 150, 214, 137, 133, 201, 245, 149, 180, 199, 247, 138,
            151, 184, 115, 183, 216, 217, 135, 167, 122, 72, 130, 132, 234, 244, 166, 197, 90, 148, 164, 198, 146, 195,
            104, 181, 200, 228, 229, 230, 233, 162, 163, 227, 194, 102, 103, 147, 170, 212, 213, 231, 248, 136, 154,
            215, 119, 196, 100, 226, 152, 165, 202, 218, 232, 243, 246, 169, 178, 179, 242, 210, 131, 186, 211,
        ],
    ),
    (
        &[0, 0, 6, 2, 1, 3, 3, 2, 5, 1, 2, 2, 8, 10, 0, 117],
        &[
            4, 5, 3, 6, 2, 7, 1, 8, 9, 18, 19, 20, 17, 21, 10, 22, 23, 240, 0, 34, 33, 24, 35, 25, 36, 50, 49, 37, 51,
            56, 55, 52, 53, 54, 57, 121, 87, 88, 89, 40, 86, 120, 39, 65, 41, 119, 38, 66, 118, 153, 26, 85, 152, 151,
            249, 72, 84, 150, 137, 71, 183, 73, 250, 117, 104, 182, 103, 105, 185, 184, 216, 82, 215, 136, 181, 116,
            81, 70, 217, 248, 58, 214, 135, 69, 122, 149, 213, 246, 134, 180, 169, 148, 83, 42, 168, 67, 245, 247, 212,
            102, 167, 90, 68, 138, 201, 232, 200, 231, 154, 106, 115, 74, 97, 199, 244, 198, 101, 233, 114, 230, 113,
            145, 147, 166, 218, 146, 133, 98, 243, 197, 178, 164, 132, 186, 100, 165, 179, 210, 129, 229, 211, 170,
            196, 202, 242, 177, 228, 209, 131, 99, 234, 195, 226, 130, 241, 163, 194, 161, 193, 227, 162, 225,
        ],
    ),
];

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
    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| (self.step() >> 24) as u8).collect()
    }
}

/// Canonical (code, length) of every symbol of a table (ITU-T T.81 Annex C).
fn codes(t: &(&[u8; 16], &[u8])) -> Vec<(u8, u32, u32)> {
    let mut out = vec![];
    let (mut code, mut k) = (0u32, 0usize);
    for len in 1..=16u32 {
        for _ in 0..t.0[len as usize - 1] {
            out.push((t.1[k], code, len));
            code += 1;
            k += 1;
        }
        code <<= 1;
    }
    out
}

struct BitWriter {
    out: Vec<u8>,
    acc: u32,
    n: u32,
}
impl BitWriter {
    fn put(&mut self, v: u32, len: u32) {
        for i in (0..len).rev() {
            self.acc = (self.acc << 1) | ((v >> i) & 1);
            self.n += 1;
            if self.n == 8 {
                let b = self.acc as u8;
                self.out.push(b);
                if b == 0xff {
                    self.out.push(0);
                }
                self.acc = 0;
                self.n = 0;
            }
        }
    }
    fn finish(mut self) -> Vec<u8> {
        while self.n != 0 {
            self.put(1, 1); // pad with ones, as JPEG does
        }
        self.out
    }
}

fn nbits(d: i32) -> u32 {
    if d == 0 { 0 } else { 32 - d.unsigned_abs().leading_zeros() }
}
fn diff_bits(d: i32, len: u32) -> u32 {
    if d < 0 { (d - 1) as u32 & ((1 << len) - 1) } else { d as u32 }
}

/// Encoder for 10-bit images (spec section 2); returns the entropy-coded stream.
fn encode(img: &[u16], w: usize, h: usize, table: usize) -> Vec<u8> {
    let first = codes(&FIRST[table]);
    let second = codes(&SECOND[table]);
    let find = |t: &[(u8, u32, u32)], leaf: u8| *t.iter().find(|c| c.0 == leaf).expect("symbol in table");
    let mut wr = BitWriter { out: vec![], acc: 0, n: 0 };
    let mut carry = 0i32;
    let mut row = 0;
    while row < h {
        let rows = (h - row).min(8);
        for block in 0..rows * w / 64 {
            let mut d = [0i32; 64];
            for (i, di) in d.iter_mut().enumerate() {
                let n = block * 64 + i; // index inside the stripe
                let x = n % w;
                let prev = if x >= 2 { img[row * w + n - 2] as i32 } else { 512 };
                *di = img[row * w + n] as i32 - prev;
            }
            let dc = d[0] - carry;
            carry = d[0];
            let len = nbits(dc);
            let (_, c, l) = find(&first, len as u8);
            wr.put(c, l);
            wr.put(diff_bits(dc, len), len);
            let last = (1..64).rev().find(|&i| d[i] != 0).unwrap_or(0);
            let mut run = 0;
            for &v in &d[1..=last] {
                if v == 0 {
                    run += 1;
                    continue;
                }
                while run > 15 {
                    let (_, c, l) = find(&second, 0xf0);
                    wr.put(c, l);
                    run -= 16;
                }
                let len = nbits(v);
                let (_, c, l) = find(&second, ((run << 4) | len) as u8);
                wr.put(c, l);
                wr.put(diff_bits(v, len), len);
                run = 0;
            }
            if last < 63 {
                let (_, c, l) = find(&second, 0);
                wr.put(c, l);
            }
        }
        row += 8;
    }
    wr.finish()
}

fn image(r: &mut Rng, w: usize, h: usize) -> Vec<u16> {
    (0..w * h)
        .map(|i| {
            let x = r.step();
            let base = 300 + ((i % w) * 3 + i / w) as u64 % 400;
            let noise = if x % 7 == 0 { (x >> 8) % 300 } else { (x >> 8) % 6 };
            (match (i / 37) % 5 {
                0 => 512,
                1 if x % 97 == 0 => (x >> 20) % 1024, // extreme jumps
                _ => (base + noise).min(1023),
            }) as u16
        })
        .collect()
}

fn kind<T>(r: &flashraw_core::Result<T>) -> &'static str {
    match r {
        Ok(_) => "ok",
        Err(e) => e.kind(),
    }
}

struct Case<'a> {
    w: usize,
    h: usize,
    table: u32,
    low: Option<&'a [u8]>,
    quirk: bool,
}

fn run(stream: &[u8], c: &Case, out_len: usize) -> (&'static str, Vec<u16>) {
    let p = new::CrwParams { width: c.w, height: c.h, table: c.table, lowbits: c.low, low_offset_quirk: c.quirk };
    let mut out = vec![0u16; out_len];
    let r = new::decode(stream, &p, &mut out);
    (kind(&r), out)
}

fn fnv(v: &[u16]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for x in v {
        for b in x.to_le_bytes() {
            h = (h ^ b as u64).wrapping_mul(0x100_0000_01b3);
        }
    }
    h
}

/// What the decoder must produce from the 10-bit image `img` and a low-bit plane (spec section 5).
fn merged(img: &[u16], low: Option<&[u8]>, quirk: bool) -> Vec<u16> {
    let Some(plane) = low else { return img.to_vec() };
    img.iter()
        .enumerate()
        .map(|(n, &v)| {
            let bits = plane.get(n / 4).copied().unwrap_or(0xff) >> (2 * (n % 4)) & 3;
            let m = (v << 2) | bits as u16;
            if quirk && m < 512 { m + 2 } else { m }
        })
        .collect()
}

const GOLD: [u64; 144] = [
    0xce1c5b51ba95b115,
    0xd0dddfeb7d0bbe8b,
    0xd0dddfeb7d0bbe8b,
    0x2183c6fef6009e89,
    0x906707ce74e1ff50,
    0xf17392761e6a3e08,
    0xf17392761e6a3e08,
    0x1b0059e18dafae67,
    0xe11ba21e2d5889dc,
    0x47d48cd8199d0813,
    0x47d48cd8199d0813,
    0x73da8de516a68e06,
    0x8b6deb267ce7dcae,
    0x42fdd1af1079328f,
    0x1107c26e1e4c4c09,
    0x16d896c9d7a937c8,
    0xf7f93a0aa4c53849,
    0xcb3afb391880df17,
    0xcb3afb391880df17,
    0x809e93950143d075,
    0x1e2977e3bfbe3b34,
    0x6c597c089fee69a3,
    0x6c597c089fee69a3,
    0x56f46505494277e3,
    0x47cc3ad74a6f41e3,
    0xf3b1a2befc1ff8d2,
    0xf3b1a2befc1ff8d2,
    0xc02c2df9632c794d,
    0xe6651cfed5842086,
    0x505971f6ecbe435e,
    0x505971f6ecbe435e,
    0xa7e50ea7028319c1,
    0x2f9242776079cf21,
    0x5189f208366b819b,
    0x5189f208366b819b,
    0xb9a726441572513d,
    0xd8d905aabed6797a,
    0x73fe191b2cc28157,
    0x73fe191b2cc28157,
    0x3a005357a1b6522c,
    0x5d45fa8c84a9bb12,
    0x6a002c799572da0f,
    0x8e807870039bb029,
    0x4d517c613c67d6ef,
    0x6e796c98b9404b6a,
    0x0b8bda56ca88c96b,
    0x0b8bda56ca88c96b,
    0xc1892f75eb57577f,
    0x987d4ded2edecfff,
    0x55c08c2082a3d20f,
    0xf1a2e9a713f7572d,
    0x439bc4118dfd1cb9,
    0xc8a6a9cdef03d273,
    0x40f527a615db6719,
    0x6c0946146743c02f,
    0xd1f2698f28d47324,
    0x7671cec435f6f683,
    0xb799b6cddd94708e,
    0x8e34b2d0bb589f04,
    0xe876a356bdb6a1b4,
    0x9e9cd0e0865b6f98,
    0xf9ef30a7371d06f4,
    0xec59375888580aa2,
    0x66186cdc54f62a48,
    0x157408b5d9555967,
    0x0ca806320cbb7b57,
    0xb75030e537b84459,
    0xff72f34beb92acba,
    0x0b3aa2a73ce762d8,
    0xa9a0fb9ab7129bc9,
    0xfacb6c2e7b30a227,
    0x7f255b4ddf85a0eb,
    0x8bced774633b2b55,
    0xff9703c69660d591,
    0xb7c2e22e8c08c2fb,
    0xd4bf1d50599e52bf,
    0x63690e565850c157,
    0x2b7d822d48724b85,
    0x7793e4f5a368d84b,
    0x793e135543381d4b,
    0x9d2368a1cab3526b,
    0xa316b217ebdedfe5,
    0xa473f0badf20d5bb,
    0x4b4c747ed0d1794a,
    0x30363c6be90ce93d,
    0xbef49011ab8d123c,
    0x35204b5b0ede8250,
    0x07ee9acfb8329bc2,
    0x7b42a2085db6096a,
    0xb2e62bb44ee8ac53,
    0x5d3c1899ffd8b129,
    0x6f159182bb379e60,
    0x65ac412c92ce7e46,
    0x2f1da601d64b0362,
    0x2f1da601d64b0362,
    0x3161a6cafa4d7184,
    0x4b01ca1f367c0829,
    0x5d38dd8f21034977,
    0x80a677ca54e93ea5,
    0xf757a2c403446f52,
    0x0a6430f7dacb5e41,
    0x27a463e3d3d043d5,
    0xd269e0b220279c38,
    0xee5d94544f5bfc6d,
    0x2539a8d956d41011,
    0x83115497f7870a00,
    0x0d4647a16a18d8d6,
    0x7dc4e7beabcd3f4a,
    0xd4caba705174ba68,
    0xbddf80d19f8b4257,
    0xb0c6a01d8c6d35b7,
    0x126b6427fdc0fd04,
    0x7903eb667e0bccf5,
    0xe1e9aebc53822104,
    0xff9dc85b0b03f8cc,
    0x6f0f8e62405911ae,
    0x8f2f0402896ea608,
    0xe7ed6f3d4c0ebe4a,
    0x5ef85c840689cb12,
    0xe4778c7ed58be3bc,
    0x80a628bb67fc00a9,
    0x9f29bd331f605222,
    0xedaba3b4440ca648,
    0xf8f7cd1f6076d9ae,
    0x95c6bef7395cf9ef,
    0x0dd21c273d74b296,
    0x69611d2a0a19a3d8,
    0x57a95c535404b8fa,
    0x9033d2b0e92bcc87,
    0x7a5f928651a6eced,
    0x47831fd58b28d3b1,
    0xd8f1f6f9eb57fdad,
    0x50a5c9e8c623bb81,
    0x364252b06fd787f0,
    0xfce7ab632cc4ce70,
    0x7a2fd9684d990a5f,
    0x292d5bc3c30509b1,
    0xc06e8be4fdce1c70,
    0xcedaf4978652c06a,
    0x2ab70f5a3c4fcd13,
    0x1924372b12cb64ef,
    0x249a3cdfec5a8479,
    0x86bd3b5fe11c9143,
    0x29874d76471b819e,
];

#[test]
fn valid_streams_round_trip_and_match_golden_hashes() {
    let mut gold = GOLD.iter();
    let mut r = Rng(0x1234_5678_9abc_def1);
    // Widths that are multiples of 8 (real files; every 8-row stripe is a whole number of blocks),
    // heights with partial last stripes, widths where rows start inside a block (w % 64 != 0).
    let geoms = [(64, 8), (64, 16), (72, 8), (136, 20), (200, 13), (128, 17), (96, 9), (1040, 11), (2144, 12)];
    for &(w, h) in &geoms {
        for table in 0..4u32 {
            let img = image(&mut r, w, h);
            let mut stream = encode(&img, w, h, table.min(2) as usize);
            // Real files continue with other data after the stream (the CIFF directory).
            let tail = 8 + r.below(200);
            stream.extend(r.bytes(tail));
            let plane = r.bytes(w * h / 4 + 1);
            let short = &plane[..plane.len() / 3];
            for (low, quirk) in
                [(None, false), (Some(&plane[..]), false), (Some(&plane[..]), true), (Some(short), true)]
            {
                let c = Case { w, h, table, low, quirk };
                let (k, out) = run(&stream, &c, w * h);
                assert_eq!(k, "ok", "valid stream rejected {w}x{h} t{table}");
                // Whole stripes of 64-sample blocks only (real files); other geometries leave the
                // uncovered tail zero, which only the golden hash pins.
                if (w * 8) % 64 == 0 && ((h % 8) * w) % 64 == 0 {
                    let want = merged(&img, low, quirk);
                    assert!(out == want, "output differs {w}x{h} table {table} low={} quirk={quirk}", low.is_some());
                }
                assert_eq!(fnv(&out), *gold.next().unwrap(), "golden hash {w}x{h} table {table}");
            }
        }
    }
    assert!(gold.next().is_none());
}

#[test]
fn all_extreme_residuals_round_trip() {
    // Images alternating between 0 and 1023 stress the longest codes and difference lengths.
    let (w, h) = (128usize, 16usize);
    for table in 0..3usize {
        for pat in 0..3 {
            let img: Vec<u16> = (0..w * h)
                .map(|i| match pat {
                    0 => {
                        if (i / 2) % 2 == 0 {
                            0
                        } else {
                            1023
                        }
                    }
                    1 => {
                        if i % 3 == 0 {
                            1023
                        } else {
                            0
                        }
                    }
                    _ => ((i * 7919) % 1024) as u16,
                })
                .collect();
            let mut s = encode(&img, w, h, table);
            s.extend([0u8; 16]);
            let c = Case { w, h, table: table as u32, low: None, quirk: false };
            let (k, out) = run(&s, &c, w * h);
            assert_eq!(k, "ok");
            assert!(out == img, "table {table} pattern {pat}");
        }
    }
}

#[test]
fn truncation_is_reported() {
    let mut r = Rng(77);
    let (w, h) = (192usize, 24usize);
    for table in 0..3usize {
        let img = image(&mut r, w, h);
        let stream = encode(&img, w, h, table);
        let c = Case { w, h, table: table as u32, low: None, quirk: false };
        for cut in [0usize, 1, 2, 17, stream.len() / 5, stream.len() / 2, stream.len() - 80] {
            let (k, out) = run(&stream[..cut], &c, w * h);
            assert_eq!(out.len(), w * h);
            assert_eq!(k, "CorruptFile", "table {table} cut {cut} of {}", stream.len());
        }
        // The complete stream without any tail either decodes exactly or is reported as truncated.
        let (k, out) = run(&stream, &c, w * h);
        assert!(k == "CorruptFile" || (k == "ok" && out == img));
    }
}

#[test]
fn garbage_never_panics_and_errors_are_corrupt_file() {
    let mut r = Rng(0xdead_beef);
    for i in 0..4000 {
        let w = [8usize, 16, 24, 64, 100, 128, 130, 2144][r.below(8)];
        let h = 1 + r.below(20);
        let len = r.below(3000);
        let mut s = r.bytes(len);
        if i % 3 == 0 {
            // Biased towards stuffing-free, short codes.
            for b in s.iter_mut() {
                if *b == 0xff {
                    *b = 0x7f;
                }
            }
        }
        let table = r.below(5) as u32;
        let plane = r.bytes(w * h / 4);
        let low = if i % 2 == 0 { Some(&plane[..]) } else { None };
        let c = Case { w, h, table, low, quirk: i % 4 == 0 };
        let (k, _) = run(&s, &c, w * h);
        assert!(k == "ok" || k == "CorruptFile", "unexpected error kind {k}");
    }
}

#[test]
fn argument_errors_have_their_kinds() {
    let s = vec![0x55u8; 64];
    for (w, h, out_len, want) in [
        (0usize, 8usize, 64usize, "InvalidArgument"),
        (8, 0, 64, "InvalidArgument"),
        (64, 8, 100, "InvalidArgument"),
        (usize::MAX, 2, 10, "CorruptFile"),
        (1 << 40, 1 << 40, 8, "CorruptFile"),
    ] {
        let c = Case { w, h, table: 0, low: None, quirk: false };
        assert_eq!(run(&s, &c, out_len).0, want, "{w}x{h} out {out_len}");
    }
}

/// Spec section 6, stated directly.
fn model_has_lowbits(f: &[u8]) -> bool {
    let end = f.len().min(16384);
    let pairs: Vec<(u8, u8)> = (540..end.saturating_sub(1)).map(|i| (f[i], f[i + 1])).collect();
    if pairs.iter().any(|&(a, b)| a == 0xff && b != 0) {
        return true;
    }
    !pairs.iter().any(|&(a, _)| a == 0xff)
}

#[test]
fn has_lowbits_follows_the_spec_on_random_files() {
    let mut r = Rng(4242);
    for i in 0..3000 {
        let len = [0usize, 1, 539, 540, 541, 600, 16383, 16384, 16385, 40000][i % 10] + r.below(3);
        let mut f = r.bytes(len);
        // Mostly no 0xFF bytes, with a few planted at interesting offsets.
        for b in f.iter_mut() {
            if *b == 0xff {
                *b = 0xfe;
            }
        }
        for _ in 0..r.below(4) {
            if len >= 2 {
                let p = [538usize, 539, 540, 541, 16382, 16383, 16384, r.below(len - 1)][r.below(8)].min(len - 1);
                f[p] = 0xff;
                if p + 1 < len {
                    f[p + 1] = if r.below(2) == 0 { 0 } else { 1 + r.below(255) as u8 };
                }
            }
        }
        assert_eq!(model_has_lowbits(&f), new::has_lowbits(&f), "len {len}");
    }
}
