//! Golden outputs of the Fuji compressed decoder on deterministic synthetic streams.
//!
//! The streams are pseudo-random bit strings wrapped in valid containers: every combination of
//! sensor layout (X-Trans, Bayer), mode (lossless, lossy), depth (12, 14, 16 bits), stripe layout
//! (a narrow last stripe, one or several stripes), quantisation bases (including above 82) and
//! truncation (the failure leaves the lines before the failing one written). The expected hashes
//! cover the status and the whole output buffer; they were recorded from the previous
//! implementation, so a mismatch is a change of behaviour.

use flashraw_codecs::fuji_compressed::{ColorTile, FujiCompressed};
use flashraw_core::Rect;

pub struct Rng(pub u64);
impl Rng {
    pub fn step(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    pub fn below(&mut self, n: u64) -> u64 {
        self.step() % n.max(1)
    }
}

pub const XTRANS: ColorTile = [
    [1, 1, 0, 1, 1, 2],
    [1, 1, 2, 1, 1, 0],
    [2, 0, 1, 0, 2, 1],
    [1, 1, 2, 1, 1, 0],
    [1, 1, 0, 1, 1, 2],
    [0, 2, 1, 2, 0, 1],
];

pub fn bayer_rggb() -> ColorTile {
    let mut o = [[1u8; 6]; 6];
    for (y, row) in o.iter_mut().enumerate() {
        for (x, v) in row.iter_mut().enumerate() {
            *v = match (y % 2, x % 2) {
                (0, 0) => 0,
                (1, 1) => 2,
                _ => 1,
            };
        }
    }
    o
}

pub fn stream_header(lossless: bool, xtrans: bool, bits: u8, width: u32, lines: u32) -> Vec<u8> {
    let stripes = width.div_ceil(768);
    let rounded = stripes * 768;
    let mut h = vec![0x49, 0x53, lossless as u8, if xtrans { 16 } else { 0 }, bits];
    h.extend_from_slice(&((lines * 6) as u16).to_be_bytes());
    h.extend_from_slice(&(rounded as u16).to_be_bytes());
    h.extend_from_slice(&(width as u16).to_be_bytes());
    h.extend_from_slice(&0x300u16.to_be_bytes());
    h.push(stripes as u8);
    h.extend_from_slice(&(lines as u16).to_be_bytes());
    h
}

/// Pseudo-random bytes with a chosen bias (long zero runs, dense ones, mixed).
pub fn noise(rng: &mut Rng, n: usize, kind: u64) -> Vec<u8> {
    (0..n)
        .map(|_| {
            let r = rng.step();
            match kind {
                0 => r as u8,
                1 => (r as u8) & (r >> 8) as u8 & (r >> 16) as u8,
                2 => (r as u8) | (r >> 8) as u8 | (r >> 16) as u8,
                3 => {
                    if r & 0xF0 == 0 {
                        0
                    } else {
                        (r >> 8) as u8
                    }
                }
                _ => {
                    if r & 0xFF00 == 0 {
                        0xFF
                    } else {
                        (r >> 16) as u8
                    }
                }
            }
        })
        .collect()
}

pub struct Spec {
    pub lossless: bool,
    pub xtrans: bool,
    pub bits: u8,
    pub width: u32,
    pub lines: u32,
    /// Quantisation bases used for the lines (cycled); ignored for lossless.
    pub q: &'static [u8],
    pub kind: u64,
    /// Bytes of entropy data per stripe, as a multiple of the lines' sample count (percent).
    pub fill_percent: usize,
    pub seed: u64,
}

pub fn build(s: &Spec) -> Vec<u8> {
    let mut rng = Rng(s.seed);
    let stripes = s.width.div_ceil(768) as usize;
    let lw = if s.xtrans { 512 } else { 384 };
    let mut d = stream_header(s.lossless, s.xtrans, s.bits, s.width, s.lines);
    let per_stripe = s.lines as usize * lw * 12 * s.fill_percent / 100;
    let bodies: Vec<Vec<u8>> = (0..stripes).map(|_| noise(&mut rng, per_stripe, s.kind)).collect();
    for b in &bodies {
        d.extend_from_slice(&(b.len() as u32).to_be_bytes());
    }
    while (d.len() - 16) % 16 != 0 {
        d.push(0);
    }
    if !s.lossless {
        let pitch = (s.lines as usize).div_ceil(16) * 16;
        for st in 0..stripes {
            for l in 0..pitch {
                d.push(s.q[(l + st) % s.q.len()]);
            }
        }
    }
    for b in &bodies {
        d.extend_from_slice(b);
    }
    d
}

const Q_LOW: &[u8] = &[0, 1, 2, 3, 5];
const Q_MID: &[u8] = &[9, 20, 40, 82];
const Q_HIGH: &[u8] = &[83, 100, 128, 200, 255];
const Q_ONE: &[u8] = &[7];

macro_rules! spec {
    ($ll:expr, $xt:expr, $bits:expr, $w:expr, $lines:expr, $q:expr, $kind:expr, $fill:expr, $seed:expr) => {
        Spec {
            lossless: $ll,
            xtrans: $xt,
            bits: $bits,
            width: $w,
            lines: $lines,
            q: $q,
            kind: $kind,
            fill_percent: $fill,
            seed: $seed,
        }
    };
}

pub fn specs() -> Vec<Spec> {
    let mut v = Vec::new();
    let mut seed = 1000;
    // Every layout x mode x depth with a narrow last stripe (792 = 768 + 24).
    for &xt in &[true, false] {
        for &ll in &[true, false] {
            for &bits in &[12u8, 14, 16] {
                seed += 1;
                v.push(spec!(ll, xt, bits, 792, 2, Q_LOW, seed % 5, 300, seed));
            }
        }
    }
    // Single and several stripes, more lines, quantisation bases of every range.
    for &xt in &[true, false] {
        for &bits in &[12u8, 14, 16] {
            for &q in &[Q_MID, Q_HIGH, Q_ONE] {
                seed += 1;
                v.push(spec!(false, xt, bits, 1560, 3, q, seed % 5, 300, seed));
            }
        }
        seed += 1;
        v.push(spec!(true, xt, 14, 768, 3, Q_ONE, 0, 300, seed));
        seed += 1;
        v.push(spec!(true, xt, 16, 2304, 1, Q_ONE, 3, 300, seed));
    }
    // Truncated stripes: a failure after some lines, an empty stripe, long zero runs.
    for &xt in &[true, false] {
        for &ll in &[true, false] {
            for &fill in &[40usize, 80, 130] {
                seed += 1;
                v.push(spec!(ll, xt, 14, 1560, 3, Q_LOW, 1, fill, seed));
            }
        }
    }
    v
}

pub fn fnv(h: &mut u64, bytes: &[u8]) {
    for &b in bytes {
        *h ^= u64::from(b);
        *h = h.wrapping_mul(0x100_0000_01b3);
    }
}

/// FNV-1a over a status byte and the output buffer, for a full decode on one thread.
pub fn full_hash(data: &[u8], tile: &ColorTile) -> u64 {
    let f = FujiCompressed::new(data).expect("container");
    let (w, h) = (f.header.width, f.header.height);
    let mut out = vec![0xA5A5u16; (w * h) as usize];
    let ok = f.decode(&mut out, w, h, tile, false).is_ok();
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    fnv(&mut hash, &[ok as u8]);
    for s in &out {
        fnv(&mut hash, &s.to_le_bytes());
    }
    hash
}

/// Recorded by the previous implementation (see the module documentation).
const GOLDEN: &[u64] = &[
    0x881e434f43a8eb84,
    0x3a9076b1e9d23615,
    0x44675e745f7a3656,
    0x3e4d7b1cb3399152,
    0x8507f51f8453a58e,
    0xadb9971119b20ab3,
    0x9ad619ff1a46b535,
    0xcf29db2819a7d15a,
    0xa20d98112cfb22a6,
    0xe46835dea2d9517e,
    0xdc9eb20197c435a8,
    0x24e0afcceede0715,
    0xa18625b1bd9f3b94,
    0x7465b50ed782753a,
    0x954d198ca20da93c,
    0xf730958f5db9b5d9,
    0x550a868ec3e801b7,
    0x0d7fa0a6b1ba7106,
    0x3b13d595fe60d1dd,
    0xbc23448d90dd3635,
    0xed0700249797194d,
    0xed53d9fccf6ce0d8,
    0xb3a6192d2b6b96b1,
    0xe551edb7c0f6b366,
    0x0c6c97fa8c784114,
    0x693f6a190485a528,
    0xaccc01131bce9ad6,
    0x68087271d51cc334,
    0x80d1b466cf584e86,
    0x08682c7c34107990,
    0x63fb7a0243c72b7d,
    0xf9ff31c8bf2a84e8,
    0x16e62ac39bd1a389,
    0x3a8dbc24c8bc8c6b,
    0xabcde9a08c275aff,
    0x53bae192ab4917be,
    0x01262407e5755e54,
    0xabcde9a08c275aff,
    0x4aaea330d3c0cb21,
    0x4940d79dc4578822,
    0xabcde9a08c275aff,
    0xabcde9a08c275aff,
    0xe6f7217c86bd26bf,
    0xabcde9a08c275aff,
    0xabcde9a08c275aff,
    0xd26c2a74ccf71ae8,
];

#[test]
fn golden_hashes() {
    let specs = specs();
    assert_eq!(GOLDEN.len(), specs.len(), "golden table out of date");
    for (i, s) in specs.iter().enumerate() {
        let data = build(s);
        let tile = if s.xtrans { XTRANS } else { bayer_rggb() };
        assert_eq!(full_hash(&data, &tile), GOLDEN[i], "spec {i}");
    }
}

/// Regions agree with the full decode, whatever the thread mode, and the parallel decode of
/// the whole image equals the sequential one.
#[test]
fn regions_and_threads_agree_with_full_decode() {
    let mut rng = Rng(77);
    for (i, s) in specs().iter().enumerate().filter(|(i, _)| i % 3 == 0) {
        let data = build(s);
        let tile = if s.xtrans { XTRANS } else { bayer_rggb() };
        let f = FujiCompressed::new(&data).unwrap();
        let (w, h) = (f.header.width, f.header.height);
        let mut full = vec![0u16; (w * h) as usize];
        let ok = f.decode(&mut full, w, h, &tile, false).is_ok();
        let mut par = vec![0u16; (w * h) as usize];
        assert_eq!(f.decode(&mut par, w, h, &tile, true).is_ok(), ok);
        if ok {
            assert_eq!(full, par, "spec {i}: parallel");
            for _ in 0..4 {
                let x = rng.below(u64::from(w)) as u32;
                let y = rng.below(u64::from(h)) as u32;
                let region = Rect {
                    x,
                    y,
                    width: 1 + rng.below(u64::from(w - x)) as u32,
                    height: 1 + rng.below(u64::from(h - y)) as u32,
                };
                let mut part = vec![0u16; (region.width * region.height) as usize];
                f.decode_region(&mut part, region, &tile, rng.below(2) == 0).unwrap();
                for ry in 0..region.height {
                    for rx in 0..region.width {
                        assert_eq!(
                            part[(ry * region.width + rx) as usize],
                            full[((region.y + ry) * w + region.x + rx) as usize],
                            "spec {i}: region {region:?} at {rx},{ry}"
                        );
                    }
                }
            }
        }
    }
}
