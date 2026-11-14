//! Property and golden tests of the X3F decoders (`flashraw_codecs::x3f`).
//!
//! Streams are produced by a small bit-level encoder written from the format description; the
//! expected samples come from a direct model of the specification (prediction on the 2 x 2
//! phase grid, running sums, global offset, CAMF packings). Hashes of the decoder outputs
//! over fixed pseudo-random cases pin the behaviour of the previous implementation, which
//! these cases were compared against bit for bit. The real-file test needs the corpus
//! (`FLASHRAW_CORPUS`) and is ignored by default.

use flashraw_codecs::x3f as new;

struct Rng(u64);
impl Rng {
    fn step(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.step() % n.max(1) as u64) as usize
    }
    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| (self.step() >> 24) as u8).collect()
    }
}

struct W {
    out: Vec<u8>,
    acc: u64,
    n: u32,
}
impl W {
    fn new() -> Self {
        W { out: Vec::new(), acc: 0, n: 0 }
    }
    fn put(&mut self, v: u32, len: u32) {
        for i in (0..len).rev() {
            self.acc = (self.acc << 1) | ((v >> i) & 1) as u64;
            self.n += 1;
            if self.n == 8 {
                self.out.push(self.acc as u8);
                self.acc = 0;
                self.n = 0;
            }
        }
    }
    fn finish(mut self) -> Vec<u8> {
        if self.n > 0 {
            self.out.push((self.acc << (8 - self.n)) as u8);
        }
        self.out
    }
}

fn kind<T>(r: &flashraw_core::Result<T>) -> &'static str {
    match r {
        Ok(_) => "ok",
        Err(e) => e.kind(),
    }
}

/// Random prefix-free code over `n` leaves (lengths up to `max_len`), possibly incomplete:
/// returns `(code, len)` per leaf, in random order.
fn random_code(r: &mut Rng, n: usize, max_len: u32) -> Vec<(u32, u32)> {
    let mut leaves: Vec<(u32, u32)> = vec![(0, 0)];
    while leaves.len() < n {
        let i = r.below(leaves.len());
        let (c, l) = leaves[i];
        if l >= max_len {
            if leaves.iter().all(|&(_, l)| l >= max_len) {
                break;
            }
            continue;
        }
        leaves[i] = (c << 1, l + 1);
        leaves.push(((c << 1) | 1, l + 1));
    }
    // A single leaf has length 0: give it length 1.
    if leaves.len() == 1 {
        leaves[0] = (0, 1);
    }
    for i in (1..leaves.len()).rev() {
        let j = r.below(i + 1);
        leaves.swap(i, j);
    }
    leaves
}

fn bitlen(d: i32) -> u32 {
    32 - d.unsigned_abs().leading_zeros()
}

fn put_diff(w: &mut W, code: &[(u32, u32)], d: i32) {
    let len = bitlen(d);
    let (c, l) = code[len as usize];
    w.put(c, l);
    if len > 0 {
        let v = if d > 0 { d as u32 } else { (d + (1i64 << len) as i32 - 1) as u32 };
        w.put(v, len);
    }
}

fn random_diff(r: &mut Rng, maxbits: u32) -> i32 {
    let b = match r.below(10) {
        0..=5 => r.below(4) as u32,
        6..=8 => r.below(8) as u32,
        _ => r.below(maxbits as usize + 1) as u32,
    };
    let b = b.min(maxbits);
    if b == 0 {
        return 0;
    }
    let mag = (1u32 << (b - 1)) | (r.step() as u32 & ((1u32 << (b - 1)) - 1));
    if r.below(2) == 0 { mag as i32 } else { -(mag as i32) }
}

/// TRUE table as stored in files: `(len, left-aligned code byte)` per symbol, then the terminator.
fn true_table(code: &[(u32, u32)]) -> Vec<(u8, u8)> {
    let mut t: Vec<(u8, u8)> = code.iter().map(|&(c, l)| (l as u8, (c << (8 - l)) as u8)).collect();
    t.push((0, 0));
    t
}

fn u16le(v: &mut Vec<u8>, x: u16) {
    v.extend_from_slice(&x.to_le_bytes());
}
fn u32le(v: &mut Vec<u8>, x: u32) {
    v.extend_from_slice(&x.to_le_bytes());
}
fn huff_setup(r: &mut Rng) -> (Vec<(u32, u32)>, Vec<u32>, Vec<u16>) {
    let nsym = 2 + r.below(300);
    let ml = if r.below(2) == 0 { 12 } else { 24 };
    let code = random_code(r, nsym, ml);
    let mut table = vec![0u32; 1024];
    let mut slots: Vec<usize> = (0..1024).collect();
    for i in (1..slots.len()).rev() {
        let j = r.below(i + 1);
        slots.swap(i, j);
    }
    let mut placed = Vec::new();
    for (k, &(c, l)) in code.iter().enumerate() {
        table[slots[k]] = (l << 27) | c;
        placed.push((slots[k], c, l));
    }
    let mapping: Vec<u16> = (0..1024)
        .map(|_| match r.below(4) {
            0 => r.step() as u16,
            1 => (r.below(40) as i32 - 20) as i16 as u16,
            2 => (r.below(400) as i32 - 200) as i16 as u16,
            _ => 0xC000,
        })
        .collect();
    // (slot, code, len) triples, but return as (code,len) with slot order kept via table
    let syms: Vec<(u32, u32)> = placed.iter().map(|&(s, _, _)| (s as u32, 0)).collect();
    (syms, table, mapping)
}

fn camf_section(kind_: u32, w: [u32; 4], data: &[u8]) -> Vec<u8> {
    let mut v = b"SECc".to_vec();
    u32le(&mut v, 0x0002_0000);
    u32le(&mut v, kind_);
    for x in w {
        u32le(&mut v, x);
    }
    v.extend_from_slice(data);
    v
}

fn camf_true_data(table: &[(u8, u8)], stream: &[u8]) -> Vec<u8> {
    let mut d = Vec::new();
    for &(l, c) in table {
        d.push(l);
        d.push(c);
    }
    d.resize(32, 0);
    d.extend_from_slice(stream);
    d.extend_from_slice(&[0; 16]);
    d
}
fn rd32(d: &[u8], o: usize) -> usize {
    u32::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]]) as usize
}

fn x3f_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            x3f_files(&p, out);
        } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("x3f")) {
            out.push(p);
        }
    }
}

struct Fnv(u64);
impl Fnv {
    fn new() -> Self {
        Fnv(0xcbf2_9ce4_8422_2325)
    }
    fn bytes(&mut self, b: &[u8]) {
        for &x in b {
            self.0 = (self.0 ^ x as u64).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    fn u16s(&mut self, v: &[u16]) {
        for &x in v {
            self.bytes(&x.to_le_bytes());
        }
    }
}

/// Plane stream plus the differences it codes (the stream is padded by 16 zero bytes).
fn plane_stream(r: &mut Rng, code: &[(u32, u32)], count: usize, maxbits: u32) -> (Vec<u8>, Vec<i32>) {
    let mut w = W::new();
    let mut diffs = Vec::new();
    for _ in 0..count {
        let d = random_diff(r, maxbits);
        put_diff(&mut w, code, d);
        diffs.push(d);
    }
    let mut v = w.finish();
    v.extend_from_slice(&[0; 16]);
    while v.len() % 16 != 0 {
        v.push(0);
    }
    (v, diffs)
}

/// The prediction of the specification, written out directly.
fn model_plane(diffs: &[i32], cols: usize, rows: usize, out_cols: usize, seed: u16) -> Vec<u16> {
    let mut s = [[seed as i64; 2]; 2];
    let mut out = Vec::new();
    let mut k = 0;
    for row in 0..rows {
        let mut a = [0i64; 2];
        for x in 0..cols {
            let (p, q) = (x % 2, row % 2);
            let d = diffs[k] as i64;
            k += 1;
            let v = if x < 2 {
                s[q][p] += d;
                s[q][p]
            } else {
                a[p] + d
            };
            a[p] = v;
            if x < out_cols {
                out.push(v as u16);
            }
        }
    }
    out
}

const GOLD_TRUE: u64 = 0xdf9f3d0186c0d098;
const GOLD_HUFFMAN: u64 = 0x3858a7b62c2e7121;
const GOLD_SIMPLE: u64 = 0xd9da47271616c720;
const GOLD_CAMF: u64 = 0xe2803473285eedd3;

fn check_gold(name: &str, got: u64, want: u64) {
    eprintln!("{name}: {got:#018x}");
    assert_eq!(got, want, "{name} golden hash");
}

#[test]
fn true_planes_match_the_model() {
    let mut r = Rng(0x1234_5678_9abc_def1);
    let mut h = Fnv::new();
    for it in 0..1500 {
        let n = 2 + r.below(13);
        let code = random_code(&mut r, n, 8);
        let n = code.len();
        let table = true_table(&code);
        let cols = 1 + r.below(40);
        let rows = 1 + r.below(12);
        let out_cols = if r.below(3) == 0 { 1 + r.below(cols) } else { cols };
        let seed = if r.below(2) == 0 { 512 } else { r.step() as u16 };
        let (data, diffs) = plane_stream(&mut r, &code, cols * rows, (n as u32 - 1).min(16));
        let hdr = new::TrueHeader { planes: None, seeds: [0; 3], table, plane_sizes: [0; 3], data_offset: 0 };
        let t = hdr.table().unwrap();
        let mut out = vec![0xA5A5u16; out_cols * rows];
        new::decode_true_plane(&t, &data, seed, cols, rows, out_cols, &mut out).unwrap();
        assert_eq!(out, model_plane(&diffs, cols, rows, out_cols, seed), "case {it}");
        h.u16s(&out);
        // A stream cut to less than half of its length cannot be complete.
        if data.len() >= 200 {
            let mut cut_out = vec![0u16; out_cols * rows];
            let res = new::decode_true_plane(&t, &data[..data.len() / 3], seed, cols, rows, out_cols, &mut cut_out);
            assert_eq!(kind(&res), "CorruptFile");
        }
    }
    check_gold("true", h.0, GOLD_TRUE);
}

#[test]
fn true_plane_argument_errors() {
    let code = random_code(&mut Rng(7), 6, 8);
    let hdr =
        new::TrueHeader { planes: None, seeds: [0; 3], table: true_table(&code), plane_sizes: [0; 3], data_offset: 0 };
    let t = hdr.table().unwrap();
    let data = [0u8; 64];
    let mut out = [0u16; 16];
    assert_eq!(kind(&new::decode_true_plane(&t, &data, 0, 4, 4, 5, &mut out)), "CorruptFile");
    assert_eq!(kind(&new::decode_true_plane(&t, &data, 0, 8, 8, 8, &mut out)), "CorruptFile");
    assert_eq!(kind(&new::decode_true_plane(&t, &data, 0, 4, 0, 4, &mut out)), "ok");
    assert_eq!(kind(&new::decode_true_plane(&t, &data, 0, 0, 4, 0, &mut out)), "ok");
    // Huge claimed geometry with tiny data fails at once instead of looping.
    let mut big = vec![0u16; 4];
    let res = new::decode_true_plane(&t, &data, 0, usize::MAX / 4, 4, 1, &mut big);
    assert_eq!(kind(&res), "CorruptFile");
    let bad = new::TrueHeader { table: vec![(9, 0), (0, 0)], ..hdr.clone() };
    assert_eq!(kind(&bad.table()), "CorruptFile");
    let none = new::TrueHeader { table: vec![(0, 0)], ..hdr };
    assert_eq!(kind(&none.table()), "CorruptFile");
}

#[test]
fn true_header_fields_and_ranges() {
    let mut r = Rng(0xfeed_0001);
    for it in 0..600 {
        let quattro = it % 2 == 0;
        let n = 2 + r.below(14);
        let code = random_code(&mut r, n, 8);
        let table = true_table(&code);
        let mut d = Vec::new();
        let planes: [(u16, u16); 3] = [(1 + r.below(7000) as u16, 1 + r.below(5000) as u16); 3];
        if quattro {
            for p in planes {
                u16le(&mut d, p.0);
                u16le(&mut d, p.1);
            }
        }
        let seeds = [r.step() as u16, r.step() as u16, r.step() as u16];
        for s in seeds {
            u16le(&mut d, s);
        }
        u16le(&mut d, 0);
        for &(l, c) in &table {
            d.push(l);
            d.push(c);
        }
        if quattro {
            u32le(&mut d, 0xdead_beef);
        }
        let sizes: [u32; 3] = [r.below(5000) as u32, r.below(5000) as u32, r.below(5000) as u32];
        for s in sizes {
            u32le(&mut d, s);
        }
        let hdr_len = d.len();
        let padded: usize = sizes.iter().map(|&s| (s as usize).div_ceil(16) * 16).sum();
        let extra = r.below(40);
        d.extend(r.bytes(padded + extra));
        let h = new::TrueHeader::parse(&d, quattro).unwrap();
        assert_eq!(h.data_offset, hdr_len);
        assert_eq!(h.seeds, seeds);
        assert_eq!(h.table, table);
        assert_eq!(h.plane_sizes, sizes);
        assert_eq!(h.planes.is_some(), quattro);
        if quattro {
            assert_eq!(h.planes.unwrap().map(|p| (p.0 as u16, p.1 as u16)), planes);
        }
        let len = d.len() - hdr_len;
        let ranges = h.plane_ranges(len).unwrap();
        let mut start = 0;
        for (i, &(s, e)) in ranges.iter().enumerate() {
            assert_eq!((s, e), (start, len));
            start += (sizes[i] as usize).div_ceil(16) * 16;
        }
        assert_eq!(kind(&h.plane_ranges(padded.saturating_sub(1))), if padded > 0 { "CorruptFile" } else { "ok" });
        h.table().unwrap();
        h.code().unwrap();
        // every cut inside the header is an error
        let cut = r.below(hdr_len);
        assert_eq!(kind(&new::TrueHeader::parse(&d[..cut], quattro)), "CorruptFile");
    }
}

/// A Huffman image from random symbols; returns data, row offsets and the expected samples.
#[allow(clippy::type_complexity)]
fn huffman_image(r: &mut Rng) -> (Vec<u32>, Vec<u16>, Vec<u8>, Vec<u32>, usize, usize, Vec<u16>) {
    let (syms, table, mapping) = huff_setup(r);
    let cols = 1 + r.below(30);
    let rows = 1 + r.below(20);
    let mut data = Vec::new();
    let mut offs = Vec::new();
    let mut sums = Vec::new();
    let mut lowest = 0i16;
    for _ in 0..rows {
        for _ in 0..r.below(3) {
            data.push(r.step() as u8);
        }
        offs.push(data.len() as u32);
        let mut w = W::new();
        let mut c = [0i16; 3];
        for _ in 0..cols {
            for ch in c.iter_mut() {
                let (slot, _) = syms[r.below(syms.len())];
                let e = table[slot as usize];
                w.put(e & 0x07FF_FFFF, e >> 27);
                *ch = ch.wrapping_add(mapping[slot as usize] as i16);
                lowest = lowest.min(*ch);
                sums.push(*ch);
            }
        }
        data.extend(w.finish());
    }
    data.extend_from_slice(&[0; 16]);
    let shift = (-(lowest as i32)) as i16; // 32768 wraps to -32768
    let expected = sums.iter().map(|&s| s.wrapping_add(shift).max(0) as u16).collect();
    (table, mapping, data, offs, cols, rows, expected)
}

#[test]
fn huffman_matches_the_model() {
    let mut r = Rng(0x0000_badc_afe1_2345);
    let mut h = Fnv::new();
    for it in 0..800 {
        let (table, mapping, data, offs, cols, rows, expected) = huffman_image(&mut r);
        let code = new::huffman_code(&table, &mapping).unwrap();
        for parallel in [false, true] {
            let mut out = vec![0x7777u16; cols * rows * 3];
            new::decode_huffman(&code, &data, &offs, cols, rows, &mut out, parallel).unwrap();
            assert_eq!(out, expected, "case {it}");
            if !parallel {
                h.u16s(&out);
            }
        }
        // a cut to a third of the data fails, never panics
        let mut out = vec![0u16; cols * rows * 3];
        let res = new::decode_huffman(&code, &data[..data.len() / 3], &offs, cols, rows, &mut out, false);
        if data.len() >= 200 {
            assert_eq!(kind(&res), "CorruptFile");
        }
    }
    check_gold("huffman", h.0, GOLD_HUFFMAN);
}

#[test]
fn huffman_wraps_the_offset_at_32768() {
    // One code (the symbol 0xC000 = -16384) used twice in a row: the sum reaches -32768, the
    // offset 32768 wraps to -32768 and every sample ends up clamped to zero or wrapped.
    let mut table = vec![0u32; 1024];
    table[3] = (1 << 27) | 1; // a one-bit code "1"
    table[4] = 1 << 27; // a one-bit code "0"
    let mut mapping = vec![0u16; 1024];
    mapping[3] = 0xC000;
    mapping[4] = 0;
    let code = new::huffman_code(&table, &mapping).unwrap();
    // pixels: three channels each: 1,1 then 0s -> sums -16384, -32768, -32768 ...
    let mut w = W::new();
    for bit in [1, 1, 1, 1, 1, 1, 0, 0, 0] {
        w.put(bit, 1);
    }
    let mut data = w.finish();
    data.extend_from_slice(&[0; 8]);
    let mut out = vec![0u16; 9];
    new::decode_huffman(&code, &data, &[0], 3, 1, &mut out, false).unwrap();
    // sums (i16): -16384 -16384 -16384 | -32768 -32768 -32768 | -32768 -32768 -32768
    let shift = -32768i16; // 32768 wrapped
    let want: Vec<u16> = [-16384i16, -16384, -16384, -32768, -32768, -32768, -32768, -32768, -32768]
        .iter()
        .map(|&s| s.wrapping_add(shift).max(0) as u16)
        .collect();
    assert_eq!(out, want);
    assert_eq!((out[0], out[3]), (16384, 0));
}

#[test]
fn huffman_garbage_does_not_panic() {
    let mut r = Rng(0x00de_adbe_ef77);
    for _ in 0..500 {
        let table: Vec<u32> =
            (0..1024)
                .map(|_| {
                    if r.below(3) == 0 { 0 } else { ((1 + r.below(31)) as u32) << 27 | (r.step() as u32 & 0x07FF_FFFF) }
                })
                .collect();
        let mapping: Vec<u16> = (0..1024).map(|_| r.step() as u16).collect();
        if let Ok(c) = new::huffman_code(&table, &mapping) {
            let data = r.bytes(200);
            let mut out = vec![0u16; 5 * 4 * 3];
            let _ = new::decode_huffman(&c, &data, &[0, 3, 100, 199], 5, 4, &mut out, false);
            let _ = new::decode_huffman(&c, &data, &[0, 3, 100, 199], 5, 4, &mut out, true);
        }
    }
    assert_eq!(kind(&new::huffman_code(&[], &[])), "CorruptFile");
    assert_eq!(kind(&new::huffman_code(&[0, 0], &[0, 0])), "CorruptFile");
}

#[test]
fn simple_matches_the_model() {
    let mut r = Rng(0x5151_5151);
    let mut h = Fnv::new();
    for it in 0..600 {
        let cols = 1 + r.below(30);
        let rows = 1 + r.below(10);
        let bits = [8u32, 9, 10, 10, 11, 12][it % 6];
        let stride = cols * 4 + 4 * r.below(3);
        let extra = r.below(8);
        let data = r.bytes(stride * rows + extra);
        let mapping: Vec<u16> = match it % 3 {
            0 => Vec::new(),
            1 => (0..1024).map(|_| r.step() as u16).collect(),
            _ => (0..1024).map(|_| (r.below(100) as i32 - 30) as i16 as u16).collect(),
        };
        let mut want = Vec::new();
        for y in 0..rows {
            let mut c = [0u16; 3];
            for x in 0..cols {
                let o = y * stride + x * 4;
                let word = u32::from_le_bytes([data[o], data[o + 1], data[o + 2], data[o + 3]]);
                for (k, ck) in c.iter_mut().enumerate() {
                    let idx = ((word >> (k as u32 * bits)) & ((1 << bits) - 1)) as usize;
                    let d = if mapping.is_empty() { idx as u16 } else { mapping.get(idx).copied().unwrap_or(0) };
                    *ck = ck.wrapping_add(d);
                    want.push(if (*ck as i16) > 0 { *ck } else { 0 });
                }
            }
        }
        for parallel in [false, true] {
            let mut out = vec![0x1111u16; cols * rows * 3];
            new::decode_simple(&data, stride, &mapping, bits, cols, rows, &mut out, parallel).unwrap();
            assert_eq!(out, want, "case {it}");
            if !parallel {
                h.u16s(&out);
            }
        }
        // data one byte short of the last row
        let mut out = vec![0u16; cols * rows * 3];
        let short = &data[..(rows - 1) * stride + cols * 4 - 1];
        assert_eq!(
            kind(&new::decode_simple(short, stride, &mapping, bits, cols, rows, &mut out, false)),
            "CorruptFile"
        );
    }
    check_gold("simple", h.0, GOLD_SIMPLE);
    let d = vec![0u8; 64];
    let mut o = [0u16; 12];
    for (stride, cols, rows, bits, outlen) in [
        (3usize, 1usize, 1usize, 10u32, 3usize),
        (4, 1, 1, 10, 2),
        (4, 0, 1, 10, 3),
        (4, 1, 0, 10, 0),
        (4, 1, 1, 7, 3),
        (4, 1, 1, 13, 3),
    ] {
        let res = new::decode_simple(&d, stride, &[], bits, cols, rows, &mut o[..outlen], false);
        assert_eq!(kind(&res), "CorruptFile", "{stride} {cols} {rows} {bits}");
    }
}

#[test]
fn prefix_codes_up_to_32_bits() {
    // A chain code: word k is k ones followed by a zero (length k+1); the last word is 31 ones.
    let mut codes = Vec::new();
    for k in 0..32u32 {
        let (bits, len) = if k < 31 { (((1u64 << k) - 1) << 1, k + 1) } else { ((1u64 << 31) - 1, 31) };
        codes.push((bits as u32, len, 1000 + k));
    }
    codes.push((0xffff_ffff, 32, 5000)); // 32 ones; the 31-ones word above is replaced by it
    codes.remove(31);
    let pc = new::PrefixCode::new(&codes).unwrap();
    let mut w = W::new();
    let order = [0u32, 31, 5, 30, 12, 31, 1, 24, 26, 25];
    for &k in &order {
        let (bits, len, _) = codes.iter().find(|c| c.2 == if k == 31 { 5000 } else { 1000 + k }).copied().unwrap();
        w.put(bits, len);
    }
    let mut data = w.finish();
    data.extend_from_slice(&[0; 8]);
    let mut bp = flashraw_core::bits::BitPumpMsb::new(&data);
    for &k in &order {
        assert_eq!(pc.decode(&mut bp).unwrap(), if k == 31 { 5000 } else { 1000 + k });
    }
    // invalid constructions
    assert_eq!(kind(&new::PrefixCode::new(&[])), "CorruptFile");
    assert_eq!(kind(&new::PrefixCode::new(&[(0, 0, 1)])), "CorruptFile");
    assert_eq!(kind(&new::PrefixCode::new(&[(0, 33, 1)])), "CorruptFile");
    assert_eq!(kind(&new::PrefixCode::new(&[(0, 1, 1), (1, 1, 2)])), "ok");
    assert_eq!(kind(&new::PrefixCode::new(&[(0, 1, 1), (2, 2, 2)])), "ok");
    assert_eq!(kind(&new::PrefixCode::new(&[(0, 1, 1), (0, 2, 2)])), "CorruptFile");
    assert_eq!(kind(&new::PrefixCode::new(&[(1, 2, 1), (1, 2, 2)])), "CorruptFile");
    // an incomplete code rejects unmatched patterns
    let inc = new::PrefixCode::new(&[(0, 2, 7)]).unwrap();
    let mut bp = flashraw_core::bits::BitPumpMsb::new(&[0xff; 8]);
    assert_eq!(kind(&inc.decode(&mut bp)), "CorruptFile");
}

fn cipher(key: u32, data: &[u8]) -> Vec<u8> {
    let mut k = key;
    data.iter()
        .map(|&b| {
            k = k.wrapping_mul(1597).wrapping_add(51749) % 244_944;
            let (k, t) = (k as u64, (k as u64 * 301_593_171) >> 24);
            b ^ ((((256 * k - t) / 2 + t) >> 17) & 255) as u8
        })
        .collect()
}

#[test]
fn camf_sections_match_the_model() {
    let mut r = Rng(0xca3f_0042);
    let mut h = Fnv::new();
    for _ in 0..200 {
        let seed = if r.below(2) == 0 { r.below(300_000) as u32 } else { r.step() as u32 };
        let dl = r.below(4000);
        let data = r.bytes(dl);
        let sec = camf_section(2, [0, 0, 0, seed], &data);
        let got = new::decode_camf(&sec).unwrap();
        let back = new::decode_camf(&camf_section(2, [0, 0, 0, seed], &got)).unwrap();
        assert_eq!(back, data, "type 2 is its own inverse");
        assert_eq!(got, cipher(seed, &data));
        h.bytes(&got);
    }
    let code = random_code(&mut r, 14, 8);
    let table = true_table(&code);
    for it in 0..300 {
        let n = 1 + r.below(3000);
        let seed = r.step() as u32;
        let mut w = W::new();
        let mut want = Vec::new();
        let mut sum = seed as u8;
        for _ in 0..n {
            let d = random_diff(&mut r, 8);
            put_diff(&mut w, &code, d);
            sum = sum.wrapping_add(d as u8);
            want.push(sum);
        }
        let data = camf_true_data(&table, &w.finish());
        let sec = camf_section(5, [n as u32, seed, 0, 0], &data);
        assert_eq!(new::decode_camf(&sec).unwrap(), want, "type 5 case {it}");
        h.bytes(&want);
        // incremental decoding gives prefixes of the whole
        let mut s = new::CamfStream::new(&sec).unwrap();
        let mut ask = 0usize;
        while !s.is_done() {
            ask += 1 + ask / 3 + 7;
            s.fill(ask).unwrap();
            assert!(s.decoded().len() >= ask.min(n));
            assert_eq!(s.decoded(), &want[..s.decoded().len()]);
        }
        assert_eq!(s.decoded(), &want[..]);
        // a block larger than the coded samples is refused or fails, never panics
        let extra = 2000 + r.below(4000);
        let big = camf_section(5, [(n + extra) as u32, seed, 0, 0], &data);
        assert_eq!(kind(&new::decode_camf(&big)), "CorruptFile");
        // truncated data: the bytes decoded from existing bits stay available
        if n >= 400 {
            let stream = data.len() - 48;
            let cut = &sec[..sec.len() - 16 - stream / 2];
            let mut s = new::CamfStream::new(cut).unwrap();
            assert_eq!(kind(&s.fill(usize::MAX)), "CorruptFile");
            assert!(!s.decoded().is_empty() && s.decoded().len() < n);
            assert_eq!(s.decoded(), &want[..s.decoded().len()]);
        }
    }
    for it in 0..300 {
        let cols = 1 + r.below(40);
        let rows = 1 + r.below(10);
        let seed = [512u32, 0, r.below(4096) as u32, r.step() as u32][it % 4];
        let mut w = W::new();
        let mut diffs = Vec::new();
        for _ in 0..cols * rows {
            let d = random_diff(&mut r, 12);
            put_diff(&mut w, &code, d);
            diffs.push(d);
        }
        let data = camf_true_data(&table, &w.finish());
        let samples = model_plane(&diffs, cols, rows, cols, seed as u16);
        let mut packed = Vec::new();
        for pair in samples.chunks(2) {
            let a = pair[0] & 0xfff;
            let b = pair.get(1).map_or(0, |&b| b & 0xfff);
            packed.push((a >> 4) as u8);
            packed.push((((a & 15) << 4) | (b >> 8)) as u8);
            if pair.len() == 2 {
                packed.push(b as u8);
            }
        }
        let size = match it % 4 {
            0 => packed.len(),
            1 => packed.len() + r.below(10),
            2 => packed.len().saturating_sub(1 + r.below(packed.len().min(9))),
            _ => (packed.len() / 2).max(1),
        };
        let mut want = packed.clone();
        want.resize(size, 0);
        let sec = camf_section(4, [size as u32, seed, cols as u32, rows as u32], &data);
        let got = new::decode_camf(&sec).unwrap();
        assert_eq!(got, want, "type 4 case {it}");
        h.bytes(&got);
        let mut s = new::CamfStream::new(&sec).unwrap();
        let mut ask = 0usize;
        while !s.is_done() {
            ask += 3 + ask / 2;
            s.fill(ask).unwrap();
            assert_eq!(s.decoded(), &want[..s.decoded().len()]);
        }
    }
    check_gold("camf", h.0, GOLD_CAMF);
}

#[test]
fn camf_header_problems_and_huge_claims() {
    for k in [0u32, 1, 3, 6, 7, 99] {
        assert_eq!(kind(&new::decode_camf(&camf_section(k, [10, 0, 1, 1], &[0; 64]))), "CorruptFile");
    }
    for len in 0..40 {
        let _ = new::decode_camf(&vec![0u8; len]);
    }
    let code = random_code(&mut Rng(3), 10, 8);
    let data = camf_true_data(&true_table(&code), &[0x55; 40]);
    // a header claiming 2^32 rows must not loop
    let sec = camf_section(4, [100, 0, u32::MAX, u32::MAX], &data);
    let _ = new::decode_camf(&sec);
    let sec = camf_section(4, [100, 0, 1, u32::MAX], &data);
    let _ = new::decode_camf(&sec);
    // zero geometry gives zero bytes
    let sec = camf_section(4, [10, 0, 0, 5], &data);
    assert_eq!(new::decode_camf(&sec).unwrap(), vec![0u8; 10]);
    // a size no data can code is refused
    let sec = camf_section(5, [u32::MAX, 0, 0, 0], &data);
    assert_eq!(kind(&new::decode_camf(&sec)), "CorruptFile");
    // no table / no data
    assert_eq!(kind(&new::decode_camf(&camf_section(5, [4, 0, 0, 0], &[1; 32]))), "CorruptFile");
    assert_eq!(kind(&new::decode_camf(&camf_section(5, [4, 0, 0, 0], &[1; 64]))), "CorruptFile");
}

#[test]
fn mutated_streams_do_not_panic() {
    let mut r = Rng(0xbad5_eed5);
    for _ in 0..400 {
        let n = 2 + r.below(13);
        let code = random_code(&mut r, n, 8);
        let n = code.len();
        let hdr = new::TrueHeader {
            planes: None,
            seeds: [0; 3],
            table: true_table(&code),
            plane_sizes: [0; 3],
            data_offset: 0,
        };
        let t = hdr.table().unwrap();
        let (cols, rows) = (1 + r.below(40), 1 + r.below(12));
        let (mut data, _) = plane_stream(&mut r, &code, cols * rows, (n as u32 - 1).min(16));
        for _ in 0..1 + r.below(4) {
            let i = r.below(data.len());
            data[i] ^= 1 << r.below(8);
        }
        data.truncate(r.below(data.len() + 1));
        let mut out = vec![0u16; cols * rows];
        let _ = new::decode_true_plane(&t, &data, 512, cols, rows, cols, &mut out);
        let sz = r.below(200) as u32;
        let sec = camf_section(4, [sz, 512, cols as u32, rows as u32], &camf_true_data(&true_table(&code), &data));
        let _ = new::decode_camf(&sec);
        let sec = camf_section(5, [sz, 7, 0, 0], &camf_true_data(&true_table(&code), &data));
        let _ = new::decode_camf(&sec);
    }
}

/// Golden hashes of the real sample files: (file name, hash of CAMF bytes and of all decoded samples).
const REAL_GOLD: &[(&str, u64)] = &[
    ("RAW_POLAROID_X530.X3F", 0x199832761d0a2223),
    ("_SDI0263.X3F", 0xacb89a00358767e9),
    ("RAW_SIGMA_DP1.X3F", 0x8296000f0b1be14c),
    ("SDIM0555.X3F", 0xb91eb6729360b5f7),
    ("_P3M9383.X3F", 0xa707db789351c615),
    ("DP2M1726.X3F", 0x5205ab188a9cdd51),
    ("DP2M5265.X3F", 0x826616b77946120b),
    ("SDIM0076.X3F", 0x19e6f83006fc3dd5),
    ("SDIM1223.X3F", 0xb445823f47769fcf),
    ("SDIM0042.X3F", 0x91bb53cf83ba039b),
    ("20120818-SDIM4579.X3F", 0xe86310602df80b17),
    ("_SDI5651.X3F", 0x82c5b9f667ed03ee),
    ("SDIM4388.X3F", 0xa2f986b984d67487),
    ("RAW_SIGMA_DP1S.X3F", 0xf477522dfa82a920),
    ("RAW_SIGMA_SD10.X3F", 0xc3659b20fdf5b3e6),
    ("RAW_SIGMA_SD9_SRGB.X3F", 0x4f36e7302298c598),
    ("sample3.X3F", 0x9e918699e5867da1),
    ("SDIM0061.X3F", 0xb066c3279b3fe02b),
];

/// Every X3F file of the corpus ($FLASHRAW_CORPUS): CAMF bytes and all decoded samples.
#[test]
#[ignore]
fn real_files() {
    let root = std::env::var_os("FLASHRAW_CORPUS").map(std::path::PathBuf::from).expect("FLASHRAW_CORPUS");
    let mut files = Vec::new();
    x3f_files(&root, &mut files);
    assert!(!files.is_empty(), "no X3F files under {root:?}");
    files.sort();
    for f in files {
        let d = std::fs::read(&f).unwrap();
        let mut h = Fnv::new();
        let dir = rd32(&d, d.len() - 4);
        let n = rd32(&d, dir + 8);
        for i in 0..n {
            let e = dir + 12 + i * 12;
            let (off, size) = (rd32(&d, e), rd32(&d, e + 4));
            let sec = &d[off..(off + size).min(d.len())];
            if &sec[..4] == b"SECc" {
                h.bytes(&new::decode_camf(sec).unwrap());
            }
            if &sec[..4] != b"SECi" || size < 28 {
                continue;
            }
            let tf = (rd32(sec, 8) << 16) + rd32(sec, 12);
            let (w, hh, stride) = (rd32(sec, 16), rd32(sec, 20), rd32(sec, 24));
            let p = &sec[28..];
            match tf {
                0x0003_0005 | 0x0003_0006 => {
                    let mapping: Vec<u16> =
                        p[..2048].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
                    let rest = &p[2048..];
                    let mut a = vec![0u16; w * hh * 3];
                    if stride != 0 {
                        new::decode_simple(rest, stride, &mapping, 10, w, hh, &mut a, true).unwrap();
                    } else {
                        let table: Vec<u32> = rest[..4096]
                            .chunks_exact(4)
                            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                            .collect();
                        let body = &rest[4096..];
                        let (coded, offs) = body.split_at(body.len() - hh * 4);
                        let offs: Vec<u32> =
                            offs.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
                        let code = new::huffman_code(&table, &mapping).unwrap();
                        new::decode_huffman(&code, coded, &offs, w, hh, &mut a, true).unwrap();
                    }
                    h.u16s(&a);
                }
                0x0003_001e | 0x0001_001e | 0x0001_0023 | 0x0001_0025 | 0x0001_0027 | 0x0001_0029 => {
                    let quattro = !matches!(tf, 0x0003_001e | 0x0001_001e);
                    let hn = new::TrueHeader::parse(p, quattro).unwrap();
                    let data = &p[hn.data_offset..];
                    let ranges = hn.plane_ranges(data.len()).unwrap();
                    let tn = hn.table().unwrap();
                    for pl in 0..3 {
                        let (cols, rows) = hn.planes.map_or((w, hh), |q| (q[pl].0 as usize, q[pl].1 as usize));
                        let out_cols = cols.min(w);
                        let (s, e) = ranges[pl];
                        let mut b = vec![0u16; out_cols * rows];
                        new::decode_true_plane(&tn, &data[s..e], hn.seeds[pl], cols, rows, out_cols, &mut b).unwrap();
                        h.u16s(&b);
                    }
                }
                _ => {}
            }
        }
        let name = f.file_name().unwrap().to_string_lossy().to_string();
        eprintln!("GOLD {name:?} {:#018x}", h.0);
        let want = REAL_GOLD.iter().find(|g| g.0 == name).map(|g| g.1);
        assert_eq!(Some(h.0), want, "{name}");
    }
}
