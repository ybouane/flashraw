//! Tests of the Panasonic decoders.
//!
//! * Property tests from the format description: block rotation, the format-4 sequential reader
//!   against the packet decoder, plain packings against a packer, format 6 against a bit-string
//!   model, format 8 against an encoder.
//! * Pinned results: seeded random inputs of every format (valid, truncated and corrupt payloads,
//!   odd sizes, odd block splits, strip layouts) are decoded and the outputs, errors and untouched
//!   cells hashed; the hashes were taken from the decoders that produced the pinned corpus hashes.
//!   Set `PANASONIC_PRINT_GOLDEN=1` to print them. `FLASHRAW_DIFF_ITERS` scales the number of cases.

use crate::panasonic as pn;
use flashraw_core::Result;

const SENTINEL: u16 = 0xA5A5;

const GOLDEN_BLOCKED: u64 = 0x2cf9833ae51f29d8;
const GOLDEN_V4_PACKET: u64 = 0xb0b6e14b87550b88;
const GOLDEN_V4_IMAGES: u64 = 0xf840e8ff8ecab355;
const GOLDEN_V4_SEQ: u64 = 0x82fd67bf8f93fff0;
const GOLDEN_V5: u64 = 0x8e9787a85c0777bd;
const GOLDEN_V6: u64 = 0x6d902befa17da921;
const GOLDEN_V7: u64 = 0xba3494a010b10c30;
const GOLDEN_V8_SMALL: u64 = 0x2f1939c611df449a;
const GOLDEN_V8_LARGE: u64 = 0x379ba9fa05345336;
const GOLDEN_V8_STRIP: u64 = 0x05131de5a050f8ca;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
    fn range(&mut self, lo: usize, hi: usize) -> usize {
        lo + self.below(hi.wrapping_sub(lo).wrapping_add(1))
    }
    fn chance(&mut self, num: usize, den: usize) -> bool {
        self.below(den) < num
    }
}

fn iters(default: usize) -> usize {
    std::env::var("FLASHRAW_DIFF_ITERS").ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

/// Random bytes in one of several styles (uniform, mostly zero, mostly 0xff, runs).
fn bytes(r: &mut Rng, len: usize) -> Vec<u8> {
    let style = r.below(4);
    let mut v: Vec<u8> = Vec::with_capacity(len);
    while v.len() < len {
        let b = r.next() as u8;
        match style {
            0 => v.push(b),
            1 => v.push(if r.chance(3, 4) { 0 } else { b }),
            2 => v.push(if r.chance(3, 4) { 0xff } else { b }),
            _ => {
                let n = r.range(1, 40).min(len - v.len());
                let fill = if r.chance(1, 2) { 0 } else { b };
                v.extend(std::iter::repeat_n(fill, n));
            }
        }
    }
    v
}

/// FNV-1a over everything a decode produced.
struct Digest(u64);
impl Digest {
    fn new() -> Self {
        Digest(0xcbf2_9ce4_8422_2325)
    }
    fn bytes(&mut self, b: &[u8]) {
        for &x in b {
            self.0 = (self.0 ^ x as u64).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    fn num(&mut self, n: u64) {
        self.bytes(&n.to_le_bytes());
    }
    fn result(&mut self, r: &Result<()>) {
        match r {
            Ok(()) => self.num(0),
            Err(e) => {
                self.num(1);
                self.bytes(e.kind().as_bytes());
                self.bytes(e.to_string().as_bytes());
            }
        }
    }
    /// Decode into a sentinel-filled buffer and digest the result and the buffer.
    fn run(&mut self, out_len: usize, f: impl FnOnce(&mut [u16]) -> Result<()>) {
        let mut buf = vec![SENTINEL; out_len];
        let r = f(&mut buf);
        self.result(&r);
        if r.is_ok() {
            self.bytes(&bytemuck_u16(&buf));
        }
    }
    fn finish(&self, name: &str, golden: u64) {
        if std::env::var_os("PANASONIC_PRINT_GOLDEN").is_some() {
            println!("GOLDEN {name} {:#018x}", self.0);
        } else if std::env::var_os("FLASHRAW_DIFF_ITERS").is_none() {
            assert_eq!(self.0, golden, "{name}: decoded results changed");
        }
    }
}

fn bytemuck_u16(v: &[u16]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

fn split(r: &mut Rng) -> usize {
    match r.below(5) {
        0 => 0,
        1 | 2 => 0x2008,
        3 => pn::BLOCK,
        _ => r.range(0, pn::BLOCK + 8), // beyond BLOCK must be rejected identically
    }
}

/// A payload of about `need` bytes: complete, short by a few bytes, cut anywhere, or longer.
fn payload(r: &mut Rng, need: usize) -> Vec<u8> {
    let len = match r.below(6) {
        0 => need.saturating_sub(r.range(1, 40)),
        1 => r.below(need + 1),
        2 => need + r.below(70000),
        _ => need,
    };
    bytes(r, len)
}

// ---------------------------------------------------------------------------------------------
// Blocked stream

/// Reference for the rotated, repeat-filled byte stream.
fn model_byte(data: &[u8], split: usize, b: usize, i: usize) -> u8 {
    let mut b = b as isize;
    while b >= 0 {
        let pos = b as usize * pn::BLOCK + (i + pn::BLOCK - split) % pn::BLOCK;
        if pos < data.len() {
            return data[pos];
        }
        b -= 1;
    }
    0
}

#[test]
fn blocked_stream_matches_the_rotation_rule() {
    let mut r = Rng(0x5eed_0001);
    let mut d = Digest::new();
    for _ in 0..iters(3000) {
        let sp = split(&mut r);
        let len = if r.chance(1, 3) { r.below(100) } else { r.below(5 * pn::BLOCK) };
        let data = bytes(&mut r, len);
        let s = match pn::BlockedStream::new(&data, sp) {
            Ok(s) => s,
            Err(e) => {
                assert!(sp > pn::BLOCK);
                assert_eq!(e.kind(), "CorruptFile");
                continue;
            }
        };
        assert!(sp <= pn::BLOCK);
        assert_eq!((s.len(), s.is_empty()), (len, len == 0));
        assert_eq!(s.packets_available(), len.div_ceil(pn::BLOCK) * pn::BLOCK / 16);
        for _ in 0..50 {
            let p = r.below(s.packets_available() + 3 * pn::BLOCK / 16 + 1);
            let pk = s.packet(p);
            for (k, &v) in pk.iter().enumerate() {
                let (b, i) = (p / (pn::BLOCK / 16), p % (pn::BLOCK / 16) * 16 + k);
                assert_eq!(v, model_byte(&data, sp % pn::BLOCK, b, i), "packet {p} byte {k} split {sp:#x} len {len}");
            }
            d.bytes(&pk);
        }
    }
    d.finish("blocked_stream", GOLDEN_BLOCKED);
}

// ---------------------------------------------------------------------------------------------
// Format 4

#[test]
fn v4_packet_fixed_layout_matches_the_field_reader() {
    // The sequential reader decodes field by field; for a width of 14 it must agree with the
    // packet decoder (which takes a fixed-layout shortcut when both channels have a base).
    let mut r = Rng(0x5eed_0004);
    let mut d = Digest::new();
    for i in 0..iters(100_000) {
        let pk: [u8; 16] = std::array::from_fn(|_| match i % 3 {
            0 => r.next() as u8,
            1 => {
                if r.chance(3, 4) {
                    0
                } else {
                    r.next() as u8
                }
            }
            _ => {
                if r.chance(1, 2) {
                    0xff
                } else {
                    r.next() as u8
                }
            }
        });
        let mut a = [SENTINEL; 14];
        pn::v4_packet(&pk, &mut a);
        // The sequential reader walks the packet from its top byte down; a one-packet payload
        // with no rotation holds it unchanged.
        let s = pn::BlockedStream::new(&pk, 0).unwrap();
        let mut b = [SENTINEL; 14];
        pn::decode_v4_sequential(&s, 14, 1, &mut b).unwrap();
        assert_eq!(a, b, "{pk:02x?}");
        d.bytes(&bytemuck_u16(&a));
    }
    d.finish("v4_packet", GOLDEN_V4_PACKET);
}

#[test]
fn v4_images_pinned() {
    let mut r = Rng(0xdead_beef_0000_0004);
    let mut d = Digest::new();
    for it in 0..iters(1500) {
        let big = it % 40 == 0;
        let width = 14 * if big { r.range(20, 80) } else { r.range(1, 6) };
        let height = if big { r.range(100, 400) } else { r.range(1, 40) };
        let sp = split(&mut r);
        let need = (width * height).div_ceil(14) * 16;
        let data = payload(&mut r, need);
        let Ok(b) = pn::BlockedStream::new(&data, sp) else { continue };
        let par = r.chance(1, 2);
        let n = width * height;
        d.run(n, |o| pn::decode_v4(&b, width, height, o, par));
        // The parallel and the sequential schedule agree.
        let mut x = vec![SENTINEL; n];
        let mut y = vec![SENTINEL; n];
        let (rx, ry) =
            (pn::decode_v4(&b, width, height, &mut x, true), pn::decode_v4(&b, width, height, &mut y, false));
        assert_eq!(rx.is_ok(), ry.is_ok());
        if rx.is_ok() {
            assert_eq!(x, y);
        }
        let first = r.below(height + 1);
        let rows = r.below(height - first + 2);
        d.run(rows * width + r.below(3), |o| pn::decode_v4_rows(&b, width, height, first, o, par));
        let len = [n.saturating_sub(1 + r.below(30)), n + r.below(20), 0][r.below(3)];
        d.run(len, |o| pn::decode_v4(&b, width, height, o, par));
    }
    d.finish("v4_images", GOLDEN_V4_IMAGES);
}

#[test]
fn v4_sequential_any_width_pinned() {
    let mut r = Rng(0xfeed_0000_5eed_0004);
    let mut d = Digest::new();
    for _ in 0..iters(1500) {
        let width = r.range(0, 120);
        let height = r.range(0, 30);
        let sp = split(&mut r);
        let need = (width * height).div_ceil(14) * 16;
        let mut data = payload(&mut r, need);
        if r.chance(1, 4) {
            data.extend(bytes(&mut r, 3 * pn::BLOCK)); // plenty of blocks: misaligned reads wrap
        }
        let Ok(b) = pn::BlockedStream::new(&data, sp) else { continue };
        let n = width * height;
        d.run(n + r.below(2), |o| pn::decode_v4_sequential(&b, width, height, o));
        let par = r.chance(1, 2);
        d.run(n, |o| pn::decode_v4(&b, width, height, o, par));
    }
    d.finish("v4_sequential", GOLDEN_V4_SEQ);
}

#[test]
fn v4_sequential_equals_packets_for_whole_packet_widths() {
    let mut r = Rng(0x5eed_0014);
    for _ in 0..iters(300) {
        let width = 14 * r.range(1, 8);
        let height = r.range(1, 20);
        let extra = r.below(40);
        let data = bytes(&mut r, (width * height / 14 * 16).max(1) + extra);
        let s = pn::BlockedStream::new(&data, 0x2008).unwrap();
        let (mut a, mut b) = (vec![0u16; width * height], vec![0u16; width * height]);
        let ra = pn::decode_v4_sequential(&s, width, height, &mut a);
        let rb = pn::decode_v4(&s, width, height, &mut b, false);
        assert_eq!(ra.is_ok(), rb.is_ok());
        if rb.is_ok() {
            assert_eq!(a, b);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Formats 5, 6, 7

#[test]
fn v5_images_pinned() {
    let mut r = Rng(0xabcd_0000_0000_0005);
    let mut d = Digest::new();
    for _ in 0..iters(2000) {
        let bits = [12u32, 14, 12, 14, 13, 10, 16, 0][r.below(8)];
        let ppp = pn::packed_pixels(if bits == 14 { 14 } else { 12 }).unwrap();
        d.num(pn::packed_pixels(bits).map_or(0, |v| v as u64));
        let width = r.range(0, 5 * ppp + 3);
        let height = r.range(0, 40);
        let sp = split(&mut r);
        let need = width.div_ceil(ppp) * height * 16;
        let data = payload(&mut r, need);
        let Ok(b) = pn::BlockedStream::new(&data, sp) else { continue };
        let par = r.chance(1, 2);
        let n = width * height;
        d.run(n, |o| pn::decode_v5(&b, bits, width, height, o, par));
        let first = r.below(height + 1);
        let rows = r.below(height - first + 2);
        d.run(rows * width, |o| pn::decode_v5_rows(&b, bits, width, height, first, o, par));
    }
    d.finish("v5_images", GOLDEN_V5);
}

fn v67_case(r: &mut Rng, six: bool, d: &mut Digest) {
    let bits = [12u32, 14, 12, 14, 13, 10, 16, 0][r.below(8)];
    let ppp = if six {
        if bits == 12 { 14 } else { 11 }
    } else {
        pn::packed_pixels(if bits == 14 { 14 } else { 12 }).unwrap()
    };
    let width = r.range(0, 4 * ppp + 5);
    let height = r.range(0, 70);
    let need = width / ppp * 16 * (height / 16 * 16);
    let data = payload(r, need);
    let par = r.chance(1, 2);
    let n = width * height;
    let first = r.below(height + 1);
    let rows = r.below(height - first + 2);
    if six {
        d.run(n, |o| pn::decode_v6(&data, bits, width, height, o, par));
        d.run(rows * width, |o| pn::decode_v6_rows(&data, bits, width, height, first, o, par));
    } else {
        d.run(n, |o| pn::decode_v7(&data, bits, width, height, o, par));
        d.run(rows * width, |o| pn::decode_v7_rows(&data, bits, width, height, first, o, par));
    }
}

#[test]
fn v6_images_pinned() {
    let mut r = Rng(0x1357_9bdf_0000_0006);
    let mut d = Digest::new();
    for _ in 0..iters(4000) {
        v67_case(&mut r, true, &mut d);
    }
    d.finish("v6_images", GOLDEN_V6);
}

#[test]
fn v7_images_pinned() {
    let mut r = Rng(0x2468_ace0_0000_0007);
    let mut d = Digest::new();
    for _ in 0..iters(4000) {
        v67_case(&mut r, false, &mut d);
    }
    d.finish("v7_images", GOLDEN_V7);
}

/// Pack `samples` LSB-first, `bits` per sample, into 16-byte packets of `ppp` samples.
fn pack_row(samples: &[u16], bits: u32, ppp: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for chunk in samples.chunks(ppp) {
        let mut v = 0u128;
        for (m, &s) in chunk.iter().enumerate() {
            v |= (s as u128 & ((1 << bits) - 1)) << (bits as usize * m);
        }
        // The unused high bits are ignored by the decoder: fill them with noise.
        v |= (0x5a5a_5a5a_5a5a_5a5a_5a5a_5a5a_5a5a_5a5au128) << (bits as usize * ppp);
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

#[test]
fn v5_and_v7_round_trip_through_a_packer() {
    let mut r = Rng(0x5eed_0057);
    for _ in 0..iters(300) {
        let bits = if r.chance(1, 2) { 12u32 } else { 14 };
        let ppp = pn::packed_pixels(bits).unwrap();
        let (width, height) = (r.range(1, 4 * ppp + 3), r.range(1, 40));
        let img: Vec<u16> = (0..width * height).map(|_| (r.next() as u16) & ((1 << bits) - 1)).collect();
        // Format 5: every row is padded to whole packets, in a rotated blocked stream.
        let logical: Vec<u8> = img.chunks(width).flat_map(|row| pack_row(row, bits, ppp)).collect();
        let split = [0usize, 0x2008, 0x4000, 100][r.below(4)];
        let mut stored = vec![0u8; logical.len().div_ceil(pn::BLOCK) * pn::BLOCK];
        for (i, &b) in logical.iter().enumerate() {
            let (blk, off) = (i / pn::BLOCK, i % pn::BLOCK);
            stored[blk * pn::BLOCK + (off + pn::BLOCK - split % pn::BLOCK) % pn::BLOCK] = b;
        }
        let s = pn::BlockedStream::new(&stored, split).unwrap();
        let mut out = vec![0u16; width * height];
        pn::decode_v5(&s, bits, width, height, &mut out, r.chance(1, 2)).unwrap();
        assert_eq!(out, img, "v5 {bits} bits {width}x{height} split {split}");
        // Format 7: rows hold whole packets only, and only whole bands of 16 rows exist.
        let (w7, h7) = (ppp * r.range(1, 5) + r.below(ppp), 16 * r.range(1, 3) + r.below(16));
        let img: Vec<u16> = (0..w7 * h7).map(|_| (r.next() as u16) & ((1 << bits) - 1)).collect();
        let banded = h7 / 16 * 16;
        let data: Vec<u8> =
            img.chunks(w7).take(banded).flat_map(|row| pack_row(&row[..w7 / ppp * ppp], bits, ppp)).collect();
        let mut out = vec![SENTINEL; w7 * h7];
        pn::decode_v7(&data, bits, w7, h7, &mut out, r.chance(1, 2)).unwrap();
        for y in 0..h7 {
            for x in 0..w7 {
                let got = out[y * w7 + x];
                if y >= banded {
                    assert_eq!(got, 0);
                } else if x < w7 / ppp * ppp {
                    assert_eq!(got, img[y * w7 + x], "v7 at {x},{y}");
                } else {
                    assert_eq!(got, SENTINEL, "trailing samples are untouched");
                }
            }
        }
    }
}

/// Format-6 block decoded bit by bit from the description.
fn v6_model(block: &[u8; 16], fourteen: bool) -> Vec<u16> {
    let v = u128::from_le_bytes(*block);
    let mut pos = 0u32;
    let mut take = |n: u32| -> u32 {
        let mut x = 0;
        for _ in 0..n {
            x = x << 1 | ((v >> (127 - pos)) & 1) as u32;
            pos += 1;
        }
        x
    };
    let (n, wide, narrow, base, limit, range, sat) = if fourteen {
        (11, 14, 10, 0x200u32, 0x2000u32, 0xffffu32, 0x3fffu32)
    } else {
        (14, 12, 8, 0x80, 0x800, 0x3fff, 0xfff)
    };
    let (mut first, mut last) = ([0u32; 2], [0u32; 2]);
    let (mut mul, mut pb) = (1u32, 0u32);
    let mut out = Vec::new();
    for j in 0..n {
        let mut e = if j < 2 {
            take(wide)
        } else {
            if j % 3 == 2 {
                let s = take(2);
                let s = if s == 3 { 4 } else { s };
                pb = base << s;
                mul = 1 << s;
            }
            take(narrow)
        };
        let c = j % 2;
        if first[c] != 0 {
            e = e.wrapping_mul(mul);
            if pb < limit && last[c] > pb {
                e = e.wrapping_add(last[c] - pb);
            }
            last[c] = e;
        } else {
            first[c] = e;
            if e != 0 {
                last[c] = e;
            } else {
                e = last[c];
            }
        }
        let x = e.wrapping_sub(15);
        out.push(if x <= range {
            x as u16 & range as u16
        } else if (e.wrapping_add(0x7fff_fff1) >> 31) & 1 == 1 {
            sat as u16
        } else {
            0
        });
    }
    out
}

#[test]
fn v6_blocks_match_the_bit_string_model() {
    let mut r = Rng(0x5eed_0006);
    for _ in 0..iters(20_000) {
        let fourteen = r.chance(1, 2);
        let ppb = if fourteen { 11 } else { 14 };
        // 16 identical-width rows of one block each.
        let block: [u8; 16] = std::array::from_fn(|_| if r.chance(1, 8) { 0 } else { r.next() as u8 });
        let data: Vec<u8> = block.repeat(16);
        let mut out = vec![0u16; ppb * 16];
        pn::decode_v6(&data, if fourteen { 14 } else { 12 }, ppb, 16, &mut out, false).unwrap();
        assert_eq!(&out[..ppb], &v6_model(&block, fourteen)[..], "{block:02x?}");
    }
}

// ---------------------------------------------------------------------------------------------
// Format 8

/// A random complete prefix code with `n` symbols assigned to random positions.
fn complete_code(r: &mut Rng, n: usize) -> Vec<(u8, u16)> {
    let mut leaves: Vec<(u8, u16)> = vec![(0, 0)];
    while leaves.len() < n {
        let i = r.below(leaves.len());
        if leaves[i].0 >= 16 {
            if leaves.iter().all(|l| l.0 >= 16) {
                break;
            }
            continue;
        }
        let (l, c) = leaves.swap_remove(i);
        leaves.push((l + 1, c << 1));
        leaves.push((l + 1, c << 1 | 1));
    }
    for i in (1..leaves.len()).rev() {
        leaves.swap(i, r.below(i + 1));
    }
    leaves
}

fn v8_params(r: &mut Rng) -> pn::V8Params {
    let n = if r.chance(1, 20) { [0, 18, 1][r.below(3)] } else { r.range(2, 17) };
    let codes: Vec<(u8, u16)> = match r.below(3) {
        0 | 1 => complete_code(r, n),
        // Arbitrary (possibly ambiguous or incomplete) tables, lengths 0..=16.
        _ => (0..n).map(|_| (r.range(0, 16) as u8, r.next() as u16)).collect(),
    };
    let shift_down: Vec<u8> = if r.chance(2, 3) {
        vec![0; r.range(0, 17)]
    } else {
        (0..r.range(0, 18)).map(|_| if r.chance(1, 2) { 0 } else { r.below(40) as u8 }).collect()
    };
    let (points, slopes) = if r.chance(2, 3) {
        ([0u32; 6], [65536u32; 6]) // identity curve
    } else {
        (std::array::from_fn(|_| r.next() as u32), std::array::from_fn(|_| r.next() as u32))
    };
    let clip = [65535u32, 65535, 16383, 4095, 0, r.below(65536) as u32, 70000][r.below(7)];
    let initial: [u16; 4] =
        std::array::from_fn(|_| if r.chance(1, 2) { r.below(4096) as u16 } else { r.next() as u16 });
    pn::V8Params { gamma_points: points, gamma_slopes: slopes, clip, initial, codes, shift_down }
}

/// Cut `width x height` into a grid of even-sized tiles; returns (left, top, w, h).
fn tiles(r: &mut Rng, width: usize, height: usize) -> Vec<(u32, u32, u32, u32)> {
    let cuts = |r: &mut Rng, total: usize, max_parts: usize| -> Vec<usize> {
        let mut c = vec![0usize];
        for _ in 0..r.below(max_parts) {
            c.push(2 * r.range(1, total / 2 - 1).max(1));
        }
        c.push(total);
        c.sort();
        c.dedup();
        c
    };
    let xs = cuts(r, width, 5);
    let ys = if r.chance(1, 3) { cuts(r, height, 3) } else { vec![0, height] };
    let mut v = Vec::new();
    for yw in ys.windows(2) {
        for xw in xs.windows(2) {
            v.push((xw[0] as u32, yw[0] as u32, (xw[1] - xw[0]) as u32, (yw[1] - yw[0]) as u32));
        }
    }
    for i in (1..v.len()).rev() {
        v.swap(i, r.below(i + 1));
    }
    v
}

fn v8_case(r: &mut Rng, width: usize, height: usize, with_errors: bool, d: &mut Digest) {
    let p = v8_params(r);
    let dec = match pn::V8Decoder::new(&p) {
        Ok(x) => x,
        Err(e) => {
            d.result(&Err(e));
            return;
        }
    };
    let mut t = tiles(r, width, height);
    if with_errors && r.chance(1, 3) {
        match r.below(5) {
            0 => {
                t.pop();
            }
            1 => t.push(t[0]),
            2 => t[0].2 += 1,
            3 => t[0].0 += 2,
            _ => t[0].3 = 0,
        }
    }
    let streams: Vec<(Vec<u8>, u64)> = t
        .iter()
        .map(|&(_, _, w, h)| {
            let len = (w as usize * h as usize) * r.range(1, 5) + 16;
            let len = if r.chance(1, 6) { r.below(len) } else { len };
            let bits = match r.below(4) {
                0 => (len * 8) as u64,
                1 => (len * 8 / 2) as u64,
                2 => r.below(len * 8 + 100) as u64,
                _ => (len * 8) as u64 + r.below(70) as u64,
            };
            (bytes(r, len), bits)
        })
        .collect();
    let strips: Vec<pn::V8Strip> = t
        .iter()
        .zip(&streams)
        .map(|(&(l, tp, w, h), (data, b))| pn::V8Strip { data, bits: *b, left: l, top: tp, width: w, height: h })
        .collect();
    let par = r.chance(1, 2);
    let len = width * height - if with_errors && r.chance(1, 20) { 1 } else { 0 };
    d.run(len, |o| dec.decode(&strips, o, width, height, par));
}

#[test]
fn v8_random_small_pinned() {
    let mut r = Rng(0x7777_0000_0000_0008);
    let mut d = Digest::new();
    for _ in 0..iters(3000) {
        let (w, h) = (2 * r.range(1, 40), 2 * r.range(1, 40));
        v8_case(&mut r, w, h, true, &mut d);
    }
    d.finish("v8_random_small", GOLDEN_V8_SMALL);
}

/// Large single strips take the speculative parallel path of the decoder (more than 1 Mi samples).
#[test]
fn v8_large_random_pinned() {
    let mut r = Rng(0x8888_0000_0000_0008);
    let mut d = Digest::new();
    for _ in 0..iters(6).min(40) {
        let (w, h) = (2 * r.range(300, 700), 2 * r.range(300, 600));
        v8_case(&mut r, w, h, false, &mut d);
    }
    d.finish("v8_large", GOLDEN_V8_LARGE);
}

#[test]
fn v8_decode_strip_direct_pinned() {
    let mut r = Rng(0x9999_0000_0000_0008);
    let mut d = Digest::new();
    for _ in 0..iters(1000) {
        let p = v8_params(&mut r);
        let Ok(dec) = pn::V8Decoder::new(&p) else { continue };
        let (w, h) = (r.range(0, 20), r.range(0, 20));
        let data = bytes(&mut r, w * h * 3 + 8);
        let bits = (data.len() * 8) as u64;
        let s = pn::V8Strip { data: &data, bits, left: 0, top: 0, width: w as u32, height: h as u32 };
        let nrows = if r.chance(1, 5) { h.saturating_sub(1) } else { h };
        let rw = if r.chance(1, 5) { w.saturating_sub(1) } else { w + r.below(3) };
        let mut buf = vec![vec![SENTINEL; rw]; nrows];
        let res = dec.decode_strip(&s, buf.iter_mut().map(|v| v.as_mut_slice()).collect());
        d.result(&res);
        if res.is_ok() {
            for row in &buf {
                d.bytes(&bytemuck_u16(row));
            }
        }
    }
    d.finish("v8_decode_strip", GOLDEN_V8_STRIP);
}

// --- a format-8 encoder, to decode real-looking streams -------------------------------------

/// LSB-first bit writer: the first bit written is bit 0 of byte 0.
struct BitWriter {
    out: Vec<u8>,
    n: usize,
}
impl BitWriter {
    fn put(&mut self, bit: bool) {
        if self.n % 8 == 0 {
            self.out.push(0);
        }
        if bit {
            *self.out.last_mut().unwrap() |= 1 << (self.n % 8);
        }
        self.n += 1;
    }
    /// `count` bits of `value`, most significant first.
    fn bits(&mut self, value: u32, count: u32) {
        for k in (0..count).rev() {
            self.put(value >> k & 1 == 1);
        }
    }
}

/// A prefix code for the categories 0..=16 with short codes for the small ones.
fn category_code() -> Vec<(u8, u16)> {
    // These lengths satisfy the Kraft equality, so the code is complete.
    let lens = [2u8, 2, 3, 3, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 14];
    let mut code = 0u32;
    let mut prev = lens[0];
    let mut out = Vec::new();
    for &l in &lens {
        code <<= l - prev;
        prev = l;
        out.push((l, code as u16));
        code += 1;
    }
    out
}

/// Encode `img` (`w x h`, even) as one strip; returns the bytes and the stream length in bits.
fn encode_strip(img: &[u16], w: usize, h: usize, initial: [u16; 4], codes: &[(u8, u16)]) -> (Vec<u8>, u64) {
    let mut bw = BitWriter { out: Vec::new(), n: 0 };
    let mut start = initial.map(i32::from);
    for q in 0..h / 2 {
        let mut cur = start;
        for bx in 0..w / 2 {
            let at = [
                (2 * q) * w + 2 * bx,
                (2 * q) * w + 2 * bx + 1,
                (2 * q + 1) * w + 2 * bx,
                (2 * q + 1) * w + 2 * bx + 1,
            ];
            for k in [0usize, 2, 1, 3] {
                let v = img[at[k]] as i32;
                let d = v - cur[k];
                cur[k] = v;
                let mag = d.unsigned_abs();
                let s = 32 - mag.leading_zeros();
                let (len, code) = codes[s as usize];
                bw.bits(code as u32, len as u32);
                if s > 0 {
                    let raw = if d > 0 { d as u32 } else { (d + (1 << s) - 1) as u32 };
                    bw.bits(raw, s);
                }
            }
            if bx == 0 {
                start = cur;
            }
        }
    }
    let bits = bw.n as u64;
    (bw.out, bits)
}

fn smooth_image(r: &mut Rng, w: usize, h: usize, max: u16) -> Vec<u16> {
    let mut img = vec![0u16; w * h];
    for y in 0..h {
        for x in 0..w {
            let base = ((x * 3 + y * 5) % 2000) as i32 + 100;
            let noise = (r.next() % 64) as i32 - 32;
            img[y * w + x] = (base + noise).clamp(0, max as i32) as u16;
        }
    }
    img
}

#[test]
fn v8_round_trips_through_an_encoder() {
    let mut r = Rng(0x5eed_0008);
    let codes = category_code();
    let params = pn::V8Params {
        clip: 4095,
        initial: [100, 200, 300, 400],
        codes: codes.clone(),
        gamma_slopes: [65536; 6],
        ..Default::default()
    };
    let dec = pn::V8Decoder::new(&params).unwrap();
    // Small images in several strips, and one large strip that takes the parallel path.
    for &(w, h, strips) in &[(8usize, 6usize, 1usize), (64, 40, 2), (130, 20, 3), (1400, 1400, 1)] {
        let img = smooth_image(&mut r, w, h, 4095);
        // Cut into `strips` column groups of even width.
        let cut = (w / strips) / 2 * 2;
        let mut parts = Vec::new();
        for k in 0..strips {
            let left = k * cut;
            let sw = if k + 1 == strips { w - left } else { cut };
            let sub: Vec<u16> = (0..h).flat_map(|y| img[y * w + left..y * w + left + sw].to_vec()).collect();
            let (data, bits) = encode_strip(&sub, sw, h, params.initial, &codes);
            parts.push((data, bits, left, sw));
        }
        let strips: Vec<pn::V8Strip> = parts
            .iter()
            .map(|(data, bits, left, sw)| pn::V8Strip {
                data,
                bits: *bits,
                left: *left as u32,
                top: 0,
                width: *sw as u32,
                height: h as u32,
            })
            .collect();
        for parallel in [false, true] {
            let mut out = vec![0u16; w * h];
            dec.decode(&strips, &mut out, w, h, parallel).unwrap();
            assert_eq!(out, img, "{w}x{h} in {} strips, parallel {parallel}", strips.len());
        }
        // A declared length that is too short is reported, and so is a damaged table entry.
        let mut short = strips.clone();
        short[0].bits /= 2;
        let mut out = vec![0u16; w * h];
        let e = dec.decode(&short, &mut out, w, h, false).unwrap_err();
        assert!(e.to_string().contains("truncated Panasonic v8 strip"), "{e}");
    }
}

#[test]
fn v8_geometry_errors() {
    let codes = category_code();
    let dec = pn::V8Decoder::new(&pn::V8Params { clip: 4095, codes, gamma_slopes: [65536; 6], ..Default::default() })
        .unwrap();
    let data = [0u8; 64];
    let strip = |left, top, width, height| pn::V8Strip { data: &data, bits: 512, left, top, width, height };
    let mut out = vec![0u16; 16 * 16];
    let msg = |strips: &[pn::V8Strip], out: &mut [u16]| dec.decode(strips, out, 16, 16, false).unwrap_err().to_string();
    assert!(msg(&[strip(0, 0, 16, 15)], &mut out).contains("invalid Panasonic v8 strip geometry"));
    assert!(msg(&[strip(0, 0, 16, 16), strip(8, 0, 8, 8)], &mut out).contains("overlapping Panasonic v8 strips"));
    assert!(msg(&[strip(0, 0, 8, 16)], &mut out).contains("do not cover the image"));
    assert!(msg(&[], &mut out).contains("output too small"));
    assert!(pn::V8Decoder::new(&pn::V8Params::default()).is_err());
}
