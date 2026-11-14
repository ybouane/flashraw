use super::*;
use flashraw_core::bytes::ByteOrder;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.next() as u8).collect()
    }
}

fn fnv(mut h: u64, data: &[u16]) -> u64 {
    for v in data {
        for b in v.to_le_bytes() {
            h = (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    h
}

const FNV_BASIS: u64 = 0xcbf2_9ce4_8422_2325;

fn skewed_curve() -> Vec<u16> {
    (0..0x4000u32).map(|i| (i * 3 + 1) as u16).collect()
}

fn identity_curve() -> Vec<u16> {
    (0..0x4000u32).map(|i| i as u16).collect()
}

/// Straightforward model of the engine: one symbol and difference at a time through the exact reader.
fn model(
    src: &[u8],
    tables: &[(usize, &DirectHuff)],
    width: usize,
    rows: usize,
    vpred: [[u16; 2]; 2],
    map: &dyn Fn(u16) -> u16,
) -> (Vec<u16>, Option<usize>, u64) {
    let mut bits = ExactBits::new(src);
    let mut out = vec![0u16; width * rows];
    let mut vp = vpred;
    let mut dead = None;
    for r in 0..rows {
        let t = tables.iter().rev().find(|(s, _)| *s <= r).unwrap().1;
        let mut h = [0u16; 2];
        for col in 0..width {
            let d = bits.diff(t) as u16;
            let c = col & 1;
            if col < 2 {
                vp[r & 1][c] = vp[r & 1][c].wrapping_add(d);
                h[c] = vp[r & 1][c];
            } else {
                h[c] = h[c].wrapping_add(d);
            }
            out[r * width + col] = map(h[c]);
        }
        if bits.is_dead() && dead.is_none() {
            dead = Some(r);
        }
    }
    (out, dead, bits.position())
}

/// Canonical codes `(symbol, code, length)` of a counts-and-symbols source.
fn codes(src: &[u8]) -> Vec<(u8, u32, u32)> {
    let (mut out, mut code, mut k) = (vec![], 0u32, 16);
    for len in 1..=16u32 {
        for _ in 0..src[len as usize - 1] {
            out.push((src[k], code, len));
            code += 1;
            k += 1;
        }
        code <<= 1;
    }
    out
}

struct BitWriter {
    out: Vec<u8>,
    acc: u64,
    n: u32,
}
impl BitWriter {
    fn put(&mut self, v: u32, len: u32) {
        for i in (0..len).rev() {
            self.acc = (self.acc << 1) | u64::from((v >> i) & 1);
            self.n += 1;
            if self.n == 8 {
                self.out.push(self.acc as u8);
                self.acc = 0;
                self.n = 0;
            }
        }
    }
    fn finish(mut self) -> Vec<u8> {
        while self.n != 0 {
            self.put(0, 1);
        }
        self.out
    }
}

fn nikon_meta(tree: usize, split_row: usize, curve: Vec<u16>, vpred: [[u16; 2]; 2]) -> NikonMeta {
    NikonMeta { version: [0x44, 0x20], vpred, tree, split_row, curve }
}

#[test]
fn symbol_diff_follows_the_jpeg_rule() {
    let got: Vec<i32> = (0..8).map(|b| symbol_diff(b, 3)).collect();
    assert_eq!(got, [-7, -6, -5, -4, 4, 5, 6, 7]);
    // len 9, 3 omitted bits, 6 transmitted: the missing bits are the midpoint pattern.
    assert_eq!(symbol_diff(0b111111, 0x39), 508);
    assert_eq!(symbol_diff(0, 0x39), 4 - 512);
    assert_eq!(symbol_diff(0, 0), 0);
    for sym in 1..16u32 {
        let len = sym;
        for b in [0u32, 1, (1 << len) - 1, 1 << (len - 1)] {
            let d = symbol_diff(b, sym);
            assert!(d.unsigned_abs() < 1 << len, "len {len} b {b}");
            assert_eq!(d >= 0, b >> (len - 1) == 1);
        }
    }
}

#[test]
fn table_sources() {
    assert!(DirectHuff::from_counts(&[0; 15]).is_err());
    assert!(DirectHuff::from_counts(&[0; 16]).is_err());
    let mut short = vec![0u8; 16];
    short[2] = 3;
    short.extend_from_slice(&[1, 2]);
    assert!(DirectHuff::from_counts(&short).is_err());
    short.extend_from_slice(&[3, 99, 99]);
    let t = DirectHuff::from_counts(&short).unwrap();
    assert_eq!(t.peek_bits(), 3);
    for (i, src) in NIKON_TREES.iter().enumerate() {
        let t = nikon_table(i).unwrap();
        let longest = src[..16].iter().rposition(|&c| c != 0).unwrap() as u32 + 1;
        assert_eq!(t.peek_bits(), longest);
    }
    // An incomplete table: the unused pattern consumes nothing and yields symbol 0.
    let mut inc = vec![0u8; 16];
    inc[1] = 1; // one 2-bit code: 00
    inc.push(5);
    let t = DirectHuff::from_counts(&inc).unwrap();
    let mut bits = ExactBits::new(&[0x40, 0x00]);
    assert_eq!(bits.huff(&t), 0);
    assert_eq!(bits.position(), 0);
    let mut bits = ExactBits::new(&[0x00, 0x00]);
    assert_eq!(bits.huff(&t), 5);
    assert_eq!(bits.position(), 2);
    // Over-subscribed counts: surplus codes are ignored, nothing panics.
    let mut over = vec![0u8; 16];
    over[0] = 3;
    over.extend_from_slice(&[1, 2, 3]);
    assert!(DirectHuff::from_counts(&over).is_ok());
}

#[test]
fn exact_reader_end_rules() {
    let mut b = ExactBits::new(&[0xa5, 0xff]);
    assert_eq!(b.get(4), 0xa);
    assert_eq!(b.get(4), 0x5);
    assert_eq!(b.position(), 8);
    assert_eq!(b.get(0), 0);
    assert!(!b.is_dead());
    // Ending exactly at the end is fine; the next bit is past it.
    assert_eq!(b.get(8), 0xff);
    assert!(!b.is_dead());
    assert_eq!(b.get(1), 0);
    assert!(b.is_dead());
    assert_eq!(b.position(), 17);
    // Reading across the end pads with zeros, then the reader is dead and stops advancing.
    let mut b = ExactBits::new(&[0xff]);
    assert_eq!(b.get(12), 0xff0);
    assert!(b.is_dead());
    assert_eq!(b.get(3), 0);
    assert_eq!(b.position(), 12);
    let t = nikon_table(2).unwrap();
    assert_eq!(b.huff(&t), 0);
    assert_eq!(b.diff(&t), 0);
    assert_eq!(b.position(), 12);
    let mut empty = ExactBits::new(&[]);
    empty.huff(&t);
    assert!(empty.is_dead());
    assert_eq!(empty.get(5), 0);
}

#[test]
fn argument_errors_and_order() {
    let t = nikon_table(2).unwrap();
    let data = [0u8; 64];
    let run = |width, rows, stride, outlen, tables: &[(usize, &DirectHuff)]| {
        let p = PredictiveParams { width, rows, tables, vpred: [[0; 2]; 2] };
        let mut out = vec![0u16; outlen];
        decode_predictive(&data, &p, &mut out, stride, |h| h).map(|_| ()).map_err(|e| e.kind())
    };
    let ok = [(0usize, &t)];
    assert!(run(8, 3, 8, 24, &ok).is_ok());
    assert!(run(8, 0, 8, 0, &ok).is_ok());
    assert_eq!(run(1, 3, 8, 24, &ok), Err("InvalidArgument"));
    assert_eq!(run(8, 3, 7, 24, &ok), Err("InvalidArgument"));
    assert_eq!(run(8, 3, 8, 23, &ok), Err("InvalidArgument"));
    assert_eq!(run(8, 3, 10, 27, &ok), Err("InvalidArgument"));
    assert!(run(8, 3, 10, 28, &ok).is_ok());
    assert_eq!(run(8, 3, 8, 24, &[]), Err("InvalidArgument"));
    assert_eq!(run(8, 3, 8, 24, &[(1, &t)]), Err("InvalidArgument"));
    assert_eq!(run(8, 0, 8, 0, &[]), Err("InvalidArgument"));
    assert_eq!(run(usize::MAX, 2, usize::MAX, 10, &ok), Err("InvalidArgument"));
}

/// Random bytes are a valid stream for the (complete) Nikon trees and for random tables: the engine
/// must agree with the model on pixels, the row where the data ran out and the final position.
#[test]
fn engine_matches_model() {
    let mut r = Rng(0x1357_9bdf);
    for case in 0..400 {
        let tree = case % 6;
        let t0 = nikon_table(tree).unwrap();
        let t1 = nikon_table((tree + 1) % 6).unwrap();
        let (width, rows) = (2 + r.below(130) as usize, 1 + r.below(40) as usize);
        let stride = width + (case % 3);
        let len = if case % 5 == 0 { r.below(30) } else { r.below((width * rows * 3) as u64) } as usize;
        let data = r.bytes(len);
        let vpred = [[r.next() as u16, r.next() as u16], [r.next() as u16, r.next() as u16]];
        let split = r.below(rows as u64 + 2) as usize;
        let tables: Vec<(usize, &DirectHuff)> =
            if case % 2 == 0 { vec![(0, &t0)] } else { vec![(0, &t0), (split, &t1)] };
        let map = |h: u16| h.rotate_left(5) ^ 0x1234;
        let (want, dead, pos) = model(&data, &tables, width, rows, vpred, &map);
        let p = PredictiveParams { width, rows, tables: &tables, vpred };
        let mut out = vec![0x7777u16; stride * rows];
        let bits = decode_predictive(&data, &p, &mut out, stride, map).unwrap();
        assert_eq!((bits.dead_row(), bits.position()), (dead, pos), "case {case}");
        for row in 0..rows {
            assert_eq!(
                out[row * stride..row * stride + width],
                want[row * width..(row + 1) * width],
                "case {case} row {row}"
            );
            assert!(out[row * stride + width..(row + 1) * stride].iter().all(|&v| v == 0x7777));
        }
    }
}

/// Direct tables the way the Pentax and Samsung callers build them (aligned spans of one symbol).
#[test]
fn direct_tables_match_model() {
    let mut r = Rng(0x2468_ace0);
    for case in 0..200 {
        let p = 8 + (case % 5) as u32;
        let size = 1usize << p;
        let mut direct = vec![0u16; size];
        let mut at = 0usize;
        let mut sym = 0u16;
        let mut len = 2u32;
        while at < size && sym < 14 {
            let l = len.min(p);
            let span = 1usize << (p - l);
            if at + span > size {
                break;
            }
            direct[at..at + span].fill((l as u16) << 8 | sym);
            at += span;
            sym += 1;
            if r.below(2) == 0 && len < p {
                len += 1;
            }
        }
        let t = DirectHuff::from_direct(p, direct);
        let (width, rows) = (2 + r.below(60) as usize, 1 + r.below(30) as usize);
        let n = r.below((width * rows * 2) as u64 + 3) as usize;
        let data = r.bytes(n);
        let (want, dead, pos) = model(&data, &[(0, &t)], width, rows, [[0; 2]; 2], &|h| h);
        let params = PredictiveParams { width, rows, tables: &[(0, &t)], vpred: [[0; 2]; 2] };
        let mut out = vec![0u16; width * rows];
        let bits = decode_predictive(&data, &params, &mut out, width, |h| h).unwrap();
        assert_eq!(out, want, "case {case}");
        assert_eq!((bits.dead_row(), bits.position()), (dead, pos));
    }
}

#[test]
fn lossless_streams_roundtrip() {
    let mut r = Rng(0x0f0f_1234);
    for tree in [2usize, 5] {
        let cs = codes(NIKON_TREES[tree]);
        let (width, rows) = (96usize, 40usize);
        let mut diffs = vec![];
        let mut w = BitWriter { out: vec![], acc: 0, n: 0 };
        for _ in 0..width * rows {
            let d = if r.below(40) == 0 { r.below(4001) as i32 - 2000 } else { r.below(41) as i32 - 20 };
            diffs.push(d);
            let len = if d == 0 { 0 } else { 32 - d.unsigned_abs().leading_zeros() };
            let (_, c, l) = cs.iter().find(|(s, _, _)| u32::from(*s) == len).unwrap();
            w.put(*c, *l);
            if len > 0 {
                w.put(if d < 0 { (d - 1) as u32 & ((1 << len) - 1) } else { d as u32 }, len);
            }
        }
        let data = w.finish();
        let vpred = [[100, 200], [300, 400]];
        let meta = NikonMeta { version: [0x46, 0x30], vpred, tree, split_row: 0, curve: identity_curve() };
        let mut out = vec![0u16; width * rows];
        decode_nikon(&data, &meta, width, rows, &mut out, width, false, data.len()).unwrap();
        let mut vp = vpred;
        let mut want = vec![0u16; width * rows];
        for r in 0..rows {
            let mut h = [0u16; 2];
            for col in 0..width {
                let d = diffs[r * width + col] as u16;
                let c = col & 1;
                if col < 2 {
                    vp[r & 1][c] = vp[r & 1][c].wrapping_add(d);
                    h[c] = vp[r & 1][c];
                } else {
                    h[c] = h[c].wrapping_add(d);
                }
                want[r * width + col] = (h[c] as i16).clamp(0, 0x3fff) as u16;
            }
        }
        assert_eq!(out, want, "tree {tree}");
        // Cut short: an error from decode_nikon, the first row that ran dry from the row API.
        let cut = &data[..data.len() / 2];
        let mut part = vec![0u16; width * rows];
        let dead = decode_nikon_rows(cut, &meta, width, rows, &mut part, width, false, 0).unwrap();
        let row = dead.expect("truncated stream must report a row");
        assert!(row > 0 && row + 1 < rows);
        assert_eq!(part[..(row) * width], out[..row * width]);
        assert!(decode_nikon(cut, &meta, width, rows, &mut part, width, false, 0).is_err());
        assert!(check_complete(Some(row), rows).is_err());
        assert!(check_complete(Some(rows - 1), rows).is_ok());
        assert!(check_complete(None, rows).is_ok());
    }
}

fn blob(order: ByteOrder, v0: u8, v1: u8, skip: usize, csize: usize, knots: impl Fn(usize) -> u16) -> Vec<u8> {
    let put = |m: &mut Vec<u8>, v: u16| {
        m.extend_from_slice(&if matches!(order, ByteOrder::Big) { v.to_be_bytes() } else { v.to_le_bytes() })
    };
    let mut m = vec![v0, v1];
    m.resize(2 + skip, 0);
    for v in [10u16, 20, 30, 40, csize as u16] {
        put(&mut m, v);
    }
    for i in 0..csize {
        put(&mut m, knots(i));
    }
    m.resize(m.len().max(600), 0);
    m
}

#[test]
fn nikon_blob_parsing() {
    for order in [ByteOrder::Big, ByteOrder::Little] {
        // Lossy generation 0x20, 14 bit: knots every 64 on the identity line interpolate to the identity.
        let m = blob(order, 0x44, 0x20, 0, 257, |i| (i * 64).min(16383) as u16);
        let meta = NikonMeta::parse(&m, order, 14).unwrap();
        assert_eq!((meta.version, meta.vpred, meta.tree, meta.split_row), ([0x44, 0x20], [[10, 20], [30, 40]], 3, 0));
        assert_eq!(meta.curve.len(), 0x4000);
        assert!(meta.curve[..16320].iter().enumerate().all(|(i, &v)| v as usize == i));
        assert!(!meta.is_lossless());
        // Generation 0x40 describes only the lowest quarter of the range, then the identity.
        let m = blob(order, 0x44, 0x40, 0, 257, |i| (i * 32) as u16);
        let meta = NikonMeta::parse(&m, order, 12).unwrap();
        assert_eq!(meta.tree, 0);
        assert!((0..1024).all(|i| meta.curve[i] as usize == i * 8));
        assert!((1025..4096).all(|i| meta.curve[i] as usize == i));
        // Doubling knots: the interpolation floors.
        let m = blob(order, 0x44, 0x20, 0, 257, |i| (i * i % 977) as u16);
        let meta = NikonMeta::parse(&m, order, 12).unwrap();
        for i in 0..4096usize {
            let (base, rem) = (i / 16, i % 16);
            let (a, b) = ((base * base % 977) as u64, ((base + 1) * (base + 1) % 977) as u64);
            if base < 256 {
                assert_eq!(meta.curve[i] as u64, (a * (16 - rem as u64) + b * rem as u64) / 16, "i {i}");
            }
        }
        // Direct table, with the extra block of the oldest bodies.
        let m = blob(order, 0x44, 0x58, 2110, 567, |i| (i * 7) as u16);
        let meta = NikonMeta::parse(&m, order, 12).unwrap();
        assert_eq!((meta.vpred, meta.tree), ([[10, 20], [30, 40]], 0));
        assert!((0..567).all(|i| meta.curve[i] as usize == i * 7));
        assert!((567..0x4000).all(|i| meta.curve[i] as usize == i));
        let m = blob(order, 0x44, 0x10, 0, 683, |i| (i * 3) as u16);
        assert!(
            NikonMeta::parse(&m, order, 14).unwrap().curve[..683].iter().enumerate().all(|(i, &v)| v as usize == i * 3)
        );
        // Lossless: the table after the predictors is ignored.
        let m = blob(order, 0x46, 0x30, 0, 34, |_| 0x1234);
        let meta = NikonMeta::parse(&m, order, 14).unwrap();
        assert!(meta.is_lossless());
        assert_eq!((meta.tree, meta.curve_max()), (5, 0x3fff));
        assert!(meta.curve.iter().enumerate().all(|(i, &v)| v as usize == i));
        assert_eq!(NikonMeta::parse(&m, order, 12).unwrap().tree, 2);
        // The split row sits at absolute offset 562.
        let mut m = blob(order, 0x44, 0x20, 0, 257, |i| (i * 64).min(16383) as u16);
        m[562..564].copy_from_slice(&if matches!(order, ByteOrder::Big) {
            17u16.to_be_bytes()
        } else {
            17u16.to_le_bytes()
        });
        assert_eq!(NikonMeta::parse(&m, order, 14).unwrap().split_row, 17);
        // Failures.
        assert_eq!(NikonMeta::parse(&m, order, 10).unwrap_err().kind(), "UnsupportedCompression");
        assert_eq!(NikonMeta::parse(&[], order, 12).unwrap_err().kind(), "CorruptFile");
        assert_eq!(NikonMeta::parse(&m[..1], order, 12).unwrap_err().kind(), "CorruptFile");
        assert_eq!(NikonMeta::parse(&m[..8], order, 12).unwrap_err().kind(), "CorruptFile");
        assert_eq!(NikonMeta::parse(&m[..12 + 2 * 100], order, 12).unwrap_err().kind(), "CorruptFile");
        assert_eq!(NikonMeta::parse(&m[..400], order, 12).unwrap_err().kind(), "CorruptFile");
    }
}

#[test]
fn nikon_decode_uses_the_second_table_and_the_curve() {
    let mut r = Rng(0xdead_beef);
    let data = r.bytes(60_000);
    let (width, rows) = (120usize, 60usize);
    for (tree, split) in [(0usize, 25usize), (3, 40), (5, 10), (0, 0), (3, 1000)] {
        let curve = skewed_curve();
        let vpred = [[1, 2], [3, 4]];
        let meta = nikon_meta(tree, split, curve.clone(), vpred);
        let t0 = nikon_table(tree).unwrap();
        let t1 = nikon_table((tree + 1).min(5)).unwrap();
        let tables: Vec<(usize, &DirectHuff)> =
            if split > 0 && tree + 1 < 6 { vec![(0, &t0), (split, &t1)] } else { vec![(0, &t0)] };
        let map = |h: u16| curve[(h as i16).clamp(0, 0x3fff) as usize];
        let (want, dead, _) = model(&data, &tables, width, rows, vpred, &map);
        let mut out = vec![0u16; width * rows];
        assert_eq!(decode_nikon_rows(&data, &meta, width, rows, &mut out, width, false, 0).unwrap(), dead);
        assert_eq!(out, want, "tree {tree} split {split}");
    }
    let mut bad = nikon_meta(0, 0, vec![0; 10], [[0; 2]; 2]);
    let mut out = vec![0u16; 16];
    assert_eq!(decode_nikon_rows(&data, &bad, 4, 4, &mut out, 4, false, 0).unwrap_err().kind(), "CorruptFile");
    bad.curve = identity_curve();
    assert!(decode_nikon_rows(&data, &bad, 4, 4, &mut out, 4, false, 0).is_ok());
}

/// Images above the parallel threshold decode identically through the speculative route
/// (including cut streams, declared lengths and strides) and match pinned hashes.
#[test]
fn parallel_route_matches_sequential() {
    let mut r = Rng(0x600d_f00d);
    let big = r.bytes(7 << 20);
    for (tree, width, rows, stride, len) in [
        (2usize, 2048usize, 2100usize, 2048usize, 5usize << 20),
        (5, 2048, 2100, 2051, 7 << 20),
        (0, 2049, 2050, 2049, 4 << 20),
        (3, 3000, 1500, 3000, 3 << 20),
        (1, 2048, 2100, 2048, 6 << 20),
        (4, 2048, 2100, 2048, 6 << 20),
    ] {
        let data = &big[..len];
        let meta = nikon_meta(tree, 0, skewed_curve(), [[9, 8], [7, 6]]);
        let mut seq = vec![0u16; stride * (rows - 1) + width];
        let mut par = seq.clone();
        let a = decode_nikon_rows(data, &meta, width, rows, &mut seq, stride, false, 0).unwrap();
        for declared in [0, len, len / 2] {
            par.fill(0);
            let b = decode_nikon_rows(data, &meta, width, rows, &mut par, stride, true, declared).unwrap();
            assert_eq!(a, b, "tree {tree} declared {declared}");
            assert!(seq == par, "tree {tree} declared {declared}");
        }
    }
}

#[test]
fn golden_hashes() {
    let mut r = Rng(0x9e37_79b9);
    let mut got = vec![];
    // Random streams through every tree, with and without a switch row and cut-off data.
    for tree in 0..6usize {
        for (k, (width, rows, len, split)) in
            [(251usize, 97usize, 60_000usize, 0usize), (64, 64, 5_000, 20), (333, 50, 700, 0)].into_iter().enumerate()
        {
            let data = r.bytes(len);
            let meta = nikon_meta(
                tree,
                split,
                if k == 1 { identity_curve() } else { skewed_curve() },
                [[r.next() as u16, 5], [6, r.next() as u16]],
            );
            let mut out = vec![0u16; width * rows];
            let dead = decode_nikon_rows(&data, &meta, width, rows, &mut out, width, false, 0).unwrap();
            got.push(fnv(fnv(FNV_BASIS, &out), &[dead.map_or(0xffff, |d| d as u16)]));
        }
    }
    // Blobs.
    for (v0, v1, bits, csize) in
        [(0x44u8, 0x20u8, 14u32, 257usize), (0x44, 0x40, 12, 257), (0x44, 0x10, 12, 683), (0x46, 0x30, 14, 34)]
    {
        let mut m = r.bytes(2700);
        m[0] = v0;
        m[1] = v1;
        m[10..12].copy_from_slice(&(csize as u16).to_be_bytes());
        for i in 0..csize {
            m[12 + 2 * i..14 + 2 * i].copy_from_slice(&(((i * 61) & 0x3fff) as u16).to_be_bytes());
        }
        let meta = NikonMeta::parse(&m, ByteOrder::Big, bits).unwrap();
        got.push(fnv(
            fnv(FNV_BASIS, &meta.curve),
            &[meta.vpred[0][0], meta.vpred[1][1], meta.split_row as u16, meta.tree as u16],
        ));
    }
    // Direct tables of 10 and 12 bits.
    for p in [10u32, 12] {
        let mut direct = vec![0u16; 1 << p];
        let mut at = 0usize;
        for (i, len) in [2u32, 2, 3, 4, 5, 6, 7, 8, 9, 10, 10, 10].iter().enumerate() {
            let span = 1usize << (p - len);
            direct[at..at + span].fill((*len as u16) << 8 | i as u16);
            at += span;
        }
        let t = DirectHuff::from_direct(p, direct);
        let data = r.bytes(9000);
        let params = PredictiveParams { width: 200, rows: 45, tables: &[(0, &t)], vpred: [[0; 2]; 2] };
        let mut out = vec![0u16; 200 * 45];
        let bits = decode_predictive(&data, &params, &mut out, 200, |h| h).unwrap();
        got.push(fnv(fnv(FNV_BASIS, &out), &[bits.dead_row().map_or(0xffff, |d| d as u16), bits.position() as u16]));
    }
    // A large image through the parallel route.
    let big = r.bytes(5 << 20);
    let meta = nikon_meta(5, 0, identity_curve(), [[0; 2]; 2]);
    let mut out = vec![0u16; 2048 * 2100];
    let dead = decode_nikon_rows(&big, &meta, 2048, 2100, &mut out, 2048, true, big.len()).unwrap();
    got.push(fnv(fnv(FNV_BASIS, &out), &[dead.map_or(0xffff, |d| d as u16)]));
    const EXPECTED: [u64; 25] = [
        0xd8ec5f6c8a279132,
        0x2e1a233787561924,
        0xb846e7ea5ed388ca,
        0xe547d1bc4a5f8d72,
        0x550d4ebbe0de96e0,
        0x7ff203633d90b501,
        0xc3d6d2db87446142,
        0x64fde9db9005005d,
        0x00bd86488d6775d4,
        0xc9f6e244673f989b,
        0x293c8264eac7011e,
        0xf2197dd31d0cdba4,
        0x620163a79a493573,
        0xcdd2970adbea203f,
        0x27fc99e17019d61d,
        0x8dd6900ad1c364a6,
        0x3c0e51326f87dbbd,
        0x3465acb71467427c,
        0x981a64e2a7989379,
        0x3bc48a8d6858f39a,
        0x5c8d979857e6d3f2,
        0xd6e565dc9ff37635,
        0xe30169b451f87c1b,
        0xe15a5035349558b6,
        0xbdf677c63f46954c,
    ];
    if got != EXPECTED {
        panic!("golden hashes: {got:#018x?}");
    }
}

/// The previous form of the curve-storage rule, kept for comparison.
fn previous_interpolation(ver0: u8, ver1: u8, step: usize, maxv: usize) -> Option<(usize, usize)> {
    let interpolated = ver0 == 0x44 && (ver1 == 0x20 || (ver1 == 0x40 && step > 3)) && step > 0;
    if !interpolated {
        return None;
    }
    Some(if ver1 == 0x40 { (step / 4, maxv / 4) } else { (step, maxv) })
}

#[test]
fn curve_storage_rule_matches_the_previous_condition() {
    for ver0 in 0..=255u8 {
        for ver1 in 0..=255u8 {
            for step in (0..=70usize).chain([255, 1000, 4095, 4096, 16383, 16384, 32767]) {
                let maxv = 0x3fff;
                let got = stored_curve_divisor(ver0, ver1, step).map(|d| (step / d, maxv / d));
                assert_eq!(got, previous_interpolation(ver0, ver1, step, maxv), "{ver0:#x} {ver1:#x} {step}");
            }
        }
    }
}

#[test]
fn unpadded_trees_build_the_same_tables_as_the_padded_ones() {
    // The previous stored form of the first tree carried one trailing zero symbol.
    let mut previous: Vec<Vec<u8>> = NIKON_TREES.iter().map(|t| t.to_vec()).collect();
    previous[0].push(0);
    for (i, p) in previous.iter().enumerate() {
        let a = DirectHuff::from_counts(p).unwrap();
        let b = nikon_table(i).unwrap();
        assert_eq!(a.peek_bits(), b.peek_bits());
        assert_eq!(a.direct, b.direct, "tree {i}");
    }
    assert!(nikon_table(6).is_err());
}
