//! 12-bit single-component sequential DCT tiles (early Blackmagic cinema DNGs).
//!
//! The stream is a baseline-style JPEG (one DQT with 16-bit entries, one DC and one AC
//! Huffman table). Blocks are decoded in raster order. The inverse transform is the
//! separable 8-point form of T.81 A.3.3 evaluated in single precision with a fixed order of
//! operations (products rounded before they are summed, sums accumulated left to right),
//! because the quantised coefficients are large enough that a different order changes
//! output samples.

use flashraw_core::bits::{BitPumpJpeg, BitRead, extend_diff};
use flashraw_core::huffman::HuffTable;
use flashraw_core::{Error, Result};

/// Natural index (row * 8 + column) of the coefficient at each zigzag position (T.81 Figure A.6).
const ZIGZAG: [u8; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20, 13, 6, 7, 14, 21,
    28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54,
    47, 55, 62, 63,
];

/// `cos(k * pi / 16) / 2` as single-precision bit patterns, k = 0..31 (the period of `(2j+1)*c`
/// reduced modulo 32). Entries 8 and 24 are the rounded-off cos(pi/2) and cos(3 pi/2), not zero.
const COS_BITS: [u32; 32] = [
    0x3F000000, 0x3EFB14BE, 0x3EEC835E, 0x3ED4DB31, 0x3EB504F3, 0x3E8E39DA, 0x3E43EF15, 0x3DC7C5C2, 0x240D3132,
    0xBDC7C5C2, 0xBE43EF15, 0xBE8E39DA, 0xBEB504F3, 0xBED4DB31, 0xBEEC835E, 0xBEFB14BE, 0xBF000000, 0xBEFB14BE,
    0xBEEC835E, 0xBED4DB31, 0xBEB504F3, 0xBE8E39DA, 0xBE43EF15, 0xBDC7C5C2, 0xA4D3C9CA, 0x3DC7C5C2, 0x3E43EF15,
    0x3E8E39DA, 0x3EB504F3, 0x3ED4DB31, 0x3EEC835E, 0x3EFB14BE,
];
/// `1 / sqrt(2)` in single precision.
const INV_SQRT2_BITS: u32 = 0x3F3504F3;
/// DC predictor of a flat block at the 12-bit mid level (the level shift folded in; the DC gain is 1/8).
const DC_START: i32 = 8 * 2048;

/// Parsed stream headers.
pub struct Dct12 {
    pub width: u32,
    pub height: u32,
    pub precision: u32,
    quant: [u16; 64],
    dc: HuffTable,
    ac: HuffTable,
    scan_offset: usize,
}

impl Dct12 {
    /// Parse the markers up to and including the first SOS. `dng_bug16` applies to the DC table.
    pub fn parse(data: &[u8], dng_bug16: bool) -> Result<Self> {
        if data.len() < 4 || data[0] != 0xFF || data[1] != 0xD8 {
            return Err(Error::corrupt("not a JPEG stream"));
        }
        let mut quant = [0u16; 64];
        let (mut dc, mut ac): (Option<HuffTable>, Option<HuffTable>) = (None, None);
        let mut frame: Option<(u32, u32, u32)> = None;
        let mut pos = 2usize;
        loop {
            // Next marker: skip to a 0xFF, then over the fill bytes.
            while data.get(pos).is_some_and(|&b| b != 0xFF) {
                pos += 1;
            }
            while data.get(pos) == Some(&0xFF) {
                pos += 1;
            }
            let Some(&marker) = data.get(pos) else { return Err(Error::corrupt("truncated JPEG header")) };
            pos += 1;
            if matches!(marker, 0xD8 | 0x01 | 0xD0..=0xD7) {
                continue;
            }
            let Some(len) = data.get(pos..pos + 2).map(|b| u16::from_be_bytes([b[0], b[1]]) as usize) else {
                return Err(Error::corrupt("truncated JPEG segment"));
            };
            if len < 2 || pos + len > data.len() {
                return Err(Error::corrupt("JPEG segment runs past the end of the data"));
            }
            let body = &data[pos + 2..pos + len];
            pos += len;
            match marker {
                0xDB => {
                    // Sixteen-bit entries in zigzag order after the Pq/Tq byte; missing ones read 0.
                    for (k, q) in quant.iter_mut().enumerate() {
                        *q = body.get(1 + 2 * k..3 + 2 * k).map_or(0, |b| u16::from_be_bytes([b[0], b[1]]));
                    }
                }
                0xC4 => {
                    let mut rest = body;
                    while rest.len() >= 17 {
                        let class = rest[0] >> 4;
                        let counts = &rest[1..17];
                        let total: usize = counts.iter().map(|&c| c as usize).sum();
                        if rest.len() < 17 + total {
                            return Err(Error::corrupt("Huffman table runs past its segment"));
                        }
                        let values = &rest[17..17 + total];
                        let mut table = HuffTable::from_counts(counts, values, class == 0)?;
                        if class == 0 {
                            table.dng_bug16 = dng_bug16;
                            dc.get_or_insert(table);
                        } else {
                            ac.get_or_insert(table);
                        }
                        rest = &rest[17 + total..];
                    }
                }
                0xC0 | 0xC1 => {
                    if body.len() < 6 {
                        return Err(Error::corrupt("short frame header"));
                    }
                    if body[5] != 1 {
                        return Err(Error::unsupported_compression("DNG", format!("{}-component DCT tile", body[5])));
                    }
                    let h = u16::from_be_bytes([body[1], body[2]]) as u32;
                    let w = u16::from_be_bytes([body[3], body[4]]) as u32;
                    frame = Some((body[0] as u32, h, w));
                }
                0xDA => {
                    let (Some((precision, height, width)), Some(dc), Some(ac)) = (frame, dc, ac) else {
                        return Err(Error::corrupt("scan before frame or tables"));
                    };
                    return Ok(Dct12 { width, height, precision, quant, dc, ac, scan_offset: pos });
                }
                _ => {}
            }
        }
    }

    /// Decode all complete 8x8 blocks in raster order: `sink(pixel_row, pixel_col, &block)` (multiples of 8).
    pub fn decode_blocks<F: FnMut(u32, u32, &[u16; 64])>(&self, data: &[u8], mut sink: F) -> Result<()> {
        let Some(scan) = data.get(self.scan_offset..) else {
            return Err(Error::corrupt("DCT scan offset past the end of the data"));
        };
        let cos = cos_table();
        let mut bits = BitPumpJpeg::new(scan);
        let mut dc_pred = DC_START;
        let mut out = [0u16; 64];
        for by in 0..self.height / 8 {
            for bx in 0..self.width / 8 {
                let mut coef = [0f32; 64];
                dc_pred = dc_pred.wrapping_add(self.dc.decode_diff(&mut bits)?.wrapping_mul(self.quant[0] as i32));
                coef[0] = dc_pred as f32;
                let mut k = 1usize;
                while k < 64 {
                    let sym = self.ac.decode_symbol(&mut bits)?;
                    let (run, size) = ((sym >> 4) as usize, sym & 15);
                    k += run;
                    if size == 0 && run < 15 {
                        break;
                    }
                    let v = if size == 0 { 0 } else { extend_diff(bits.get(size), size) };
                    let p = k.min(63);
                    coef[ZIGZAG[p] as usize] = v.wrapping_mul(self.quant[p] as i32) as f32;
                    k += 1;
                }
                if bits.overrun() > 64 {
                    return Err(Error::corrupt("DCT data ends early"));
                }
                inverse_dct(&mut coef, &cos, &mut out);
                sink(by * 8, bx * 8, &out);
            }
        }
        Ok(())
    }
}

/// `K[(2j+1) c mod 32]` indexed `[c][j]`: the factor of coefficient `c` in output `j` of a 1-D pass.
fn cos_table() -> [[f32; 8]; 8] {
    std::array::from_fn(|c| std::array::from_fn(|j| f32::from_bits(COS_BITS[((2 * j + 1) * c) % 32])))
}

/// 8-wide version of [`inverse_dct_body`]: the same operations on wider vectors (no fused multiply-add).
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn inverse_dct_avx2(f: &mut [f32; 64], k: &[[f32; 8]; 8], out: &mut [u16; 64]) {
    inverse_dct_body(f, k, out)
}

fn inverse_dct(f: &mut [f32; 64], k: &[[f32; 8]; 8], out: &mut [u16; 64]) {
    #[cfg(target_arch = "x86_64")]
    if std::arch::is_x86_feature_detected!("avx2") {
        // SAFETY: the CPU supports AVX2.
        unsafe { inverse_dct_avx2(f, k, out) };
        return;
    }
    inverse_dct_body(f, k, out)
}

/// Two-pass inverse DCT in single precision, in the order the format's reference output needs.
/// Each output keeps its own left-to-right chain of rounded products; the eight outputs of a
/// pass are independent, so they are computed side by side.
#[inline(always)]
fn inverse_dct_body(f: &mut [f32; 64], k: &[[f32; 8]; 8], out: &mut [u16; 64]) {
    let s = f32::from_bits(INV_SQRT2_BITS);
    for c in 0..8 {
        f[c] *= s;
    }
    for r in 0..8 {
        f[r * 8] *= s;
    }
    // Horizontal pass: t[i][j] = sum_c f[i][c] * K[(2j+1)c], accumulated in order of c.
    let mut t = [0f32; 64];
    for i in 0..8 {
        let row = &f[i * 8..i * 8 + 8];
        let mut acc = [0f32; 8];
        for c in 0..8 {
            let v = row[c];
            for j in 0..8 {
                acc[j] += v * k[c][j];
            }
        }
        t[i * 8..i * 8 + 8].copy_from_slice(&acc);
    }
    // Vertical pass: y[i][j] = sum_c t[c][j] * K[(2i+1)c]; the factor is K's [c][i] entry.
    for i in 0..8 {
        let mut acc = [0f32; 8];
        for c in 0..8 {
            let factor = k[c][i];
            for j in 0..8 {
                acc[j] += t[c * 8 + j] * factor;
            }
        }
        // Round to the nearest integer (half up, truncating toward zero) and clamp.
        for j in 0..8 {
            out[i * 8 + j] = ((acc[j] as f64 + 0.5) as i64).clamp(0, 65535) as u16;
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::ljpeg::tests::{Rng, fnv};

    /// MSB-first bit writer with JPEG byte stuffing.
    struct Writer {
        out: Vec<u8>,
        acc: u32,
        n: u32,
    }
    impl Writer {
        fn put(&mut self, v: u32, len: u32) {
            for i in (0..len).rev() {
                self.acc = (self.acc << 1) | ((v >> i) & 1);
                self.n += 1;
                if self.n == 8 {
                    self.out.push(self.acc as u8);
                    if self.acc as u8 == 0xFF {
                        self.out.push(0);
                    }
                    self.acc = 0;
                    self.n = 0;
                }
            }
        }
        /// Magnitude category and the extra bits of a signed amplitude.
        fn amplitude(&mut self, v: i32, size: u32) {
            let bits = if v < 0 { v - 1 } else { v } as u32 & ((1u32 << size) - 1);
            self.put(bits, size);
        }
        fn finish(mut self) -> Vec<u8> {
            while self.n != 0 {
                self.put(1, 1);
            }
            self.out
        }
    }

    /// AC symbols in code order (all codes 8 bits long): EOB, ZRL, then run/size pairs.
    fn ac_symbols() -> Vec<u8> {
        let mut v = vec![0x00u8, 0xF0];
        for run in 0..16u8 {
            for size in 1..=10u8 {
                v.push(run << 4 | size);
            }
        }
        v
    }

    /// Header of a 12-bit tile of `w` x `h` pixels with quantiser `q` (every entry) and `q0` for DC.
    fn header(w: usize, h: usize, q0: u16, q: u16, sof: u8) -> Vec<u8> {
        let mut out = vec![0xFF, 0xD8, 0xFF, 0xFE, 0, 4, 0x41, 0x42];
        out.extend([0xFF, 0xDB, 0, 0x83, 0x10]);
        for k in 0..64 {
            out.extend((if k == 0 { q0 } else { q }).to_be_bytes());
        }
        let ac = ac_symbols();
        out.extend([0xFF, 0xC4]);
        out.extend(((2 + 17 + 12 + 17 + ac.len()) as u16).to_be_bytes());
        out.push(0x00);
        let mut counts = [0u8; 16];
        counts[3] = 12;
        out.extend(counts);
        out.extend(0..12u8);
        out.push(0x10);
        let mut counts = [0u8; 16];
        counts[7] = ac.len() as u8;
        out.extend(counts);
        out.extend(&ac);
        out.extend([0xFF, sof, 0, 11, 12]);
        out.extend((h as u16).to_be_bytes());
        out.extend((w as u16).to_be_bytes());
        out.extend([1, 1, 0x11, 0]);
        out.extend([0xFF, 0xDA, 0, 8, 1, 1, 0, 0, 63, 0]);
        out
    }

    fn category(v: i32) -> u32 {
        32 - v.unsigned_abs().leading_zeros()
    }

    /// A tile with random quantised blocks; `dc_only` gives blocks with just a DC difference.
    fn tile(r: &mut Rng, w: usize, h: usize, sparse: bool) -> Vec<u8> {
        let q0 = [1u16, 16, 40, 65535][r.below(4) as usize];
        let q = [1u16, 12, 255, 65535][r.below(4) as usize];
        let mut out = header(w, h, q0, q, 0xC1);
        let mut wr = Writer { out: Vec::new(), acc: 0, n: 0 };
        let ac = ac_symbols();
        for _ in 0..(w / 8) * (h / 8) {
            let size = r.below(if sparse { 5 } else { 12 }) as u32;
            let dc = if size == 0 { 0 } else { (1i32 << (size - 1)) + r.below(1 << (size - 1)) as i32 };
            let dc = if r.below(2) == 0 { -dc } else { dc };
            wr.put(category(dc), 4);
            if dc != 0 {
                wr.amplitude(dc, category(dc));
            }
            let mut k = 1usize;
            while k < 64 {
                if r.below(100) < if sparse { 40 } else { 6 } {
                    break;
                }
                let run = r.below(15.min(64 - k) as u64) as usize;
                let size = 1 + r.below(if sparse { 3 } else { 10 }) as u32;
                let v = (1i32 << (size - 1)) + r.below(1 << (size - 1)) as i32;
                let v = if r.below(2) == 0 { -v } else { v };
                let sym = (run as u8) << 4 | size as u8;
                wr.put(ac.iter().position(|&s| s == sym).unwrap() as u32, 8);
                wr.amplitude(v, size);
                k += run + 1;
            }
            if k < 64 {
                wr.put(0, 8);
            }
        }
        out.extend(wr.finish());
        out.extend([0xFF, 0xD9]);
        out
    }

    #[test]
    fn parse_rejects_garbage() {
        assert!(Dct12::parse(&[0, 1, 2, 3], true).is_err());
        assert!(Dct12::parse(&[0xFF, 0xD8, 0xFF, 0xDA, 0, 2], true).is_err());
        assert!(Dct12::parse(&[], true).is_err());
        // A header without frame or tables cannot start a scan.
        assert!(Dct12::parse(&[0xFF, 0xD8, 0xFF, 0xDA, 0, 8, 1, 1, 0, 0, 63, 0], true).is_err());
    }

    #[test]
    fn cosine_table_is_the_rounded_double_precision_cosine() {
        for (k, &bits) in COS_BITS.iter().enumerate() {
            let want = ((k as f64 * std::f64::consts::PI / 16.0).cos() / 2.0) as f32;
            assert_eq!(bits, want.to_bits(), "K[{k}]");
        }
        assert_eq!(INV_SQRT2_BITS, ((1.0f64 / 2.0f64.sqrt()) as f32).to_bits());
        let mut zz = ZIGZAG;
        zz.sort_unstable();
        assert!(zz.iter().enumerate().all(|(i, &v)| v as usize == i));
    }

    #[test]
    fn dc_only_block_is_a_flat_plane() {
        // DC predictor 16384 + 100 * 16 = 17984: the plane is 2048 + 17984/8 - 2048 = 2248.
        let mut data = header(8, 8, 16, 16, 0xC1);
        let mut wr = Writer { out: Vec::new(), acc: 0, n: 0 };
        wr.put(category(100), 4);
        wr.amplitude(100, category(100));
        wr.put(0, 8);
        data.extend(wr.finish());
        data.extend([0xFF, 0xD9]);
        let d = Dct12::parse(&data, true).unwrap();
        assert_eq!((d.width, d.height, d.precision), (8, 8, 12));
        let mut seen = 0;
        d.decode_blocks(&data, |y, x, b| {
            assert_eq!((y, x), (0, 0));
            assert!(b.iter().all(|&v| v == 2248), "{b:?}");
            seen += 1;
        })
        .unwrap();
        assert_eq!(seen, 1);
    }

    #[test]
    fn truncated_data_delivers_the_blocks_before_the_error() {
        let mut r = Rng(4242);
        let data = tile(&mut r, 64, 64, false);
        let start = Dct12::parse(&data, true).unwrap().scan_offset;
        let cut = &data[..start + (data.len() - start) / 4];
        let d = Dct12::parse(cut, true).unwrap();
        let mut n = 0;
        assert!(d.decode_blocks(cut, |_, _, _| n += 1).is_err());
        assert!(n > 0 && n < 64);
    }

    #[test]
    fn wide_and_portable_transforms_agree() {
        let mut r = Rng(99);
        let k = cos_table();
        for case in 0..2000 {
            let mut a = [0f32; 64];
            for v in a.iter_mut() {
                if r.below(3) == 0 {
                    *v = (r.next() as i32 >> (8 + case % 12)) as f32;
                }
            }
            let mut b = a;
            let (mut x, mut y) = ([0u16; 64], [0u16; 64]);
            inverse_dct(&mut a, &k, &mut x);
            inverse_dct_body(&mut b, &k, &mut y);
            assert!(x == y, "case {case}");
        }
    }

    #[test]
    fn two_component_frames_are_unsupported_and_header_garbage_does_not_panic() {
        let mut data = header(16, 16, 1, 1, 0xC1);
        let sof = data.windows(2).position(|p| p == [0xFF, 0xC1]).unwrap();
        data[sof + 9] = 2;
        assert!(matches!(Dct12::parse(&data, true), Err(Error::UnsupportedCompression { .. })));
        let mut r = Rng(3);
        let base = tile(&mut r, 24, 16, false);
        for _ in 0..2000 {
            let mut d = base.clone();
            for _ in 0..3 {
                let p = r.below(d.len() as u64) as usize;
                d[p] = r.next() as u8;
            }
            if let Ok(t) = Dct12::parse(&d, true) {
                let _ = t.decode_blocks(&d, |_, _, _| {});
            }
        }
    }

    // ------------------------------------------------------------ golden digest

    /// What the decoder delivered for one stream.
    pub(crate) struct Outcome {
        pub parse: std::result::Result<(u32, u32, u32), &'static str>,
        pub blocks: Vec<(u32, u32, [u16; 64])>,
        pub error: Option<&'static str>,
    }

    pub(crate) fn streams() -> Vec<Vec<u8>> {
        let mut r = Rng(0xD0C7_1200);
        let mut out = Vec::new();
        for i in 0..300 {
            let (w, h) = (8 * (1 + r.below(6) as usize), 8 * (1 + r.below(5) as usize));
            let mut d = tile(&mut r, w, h, i % 3 == 0);
            match i % 4 {
                1 => d.truncate(d.len() * (1 + r.below(8) as usize) / 9),
                2 => {
                    let p = 150 + r.below(d.len() as u64 - 150) as usize;
                    d[p] ^= 1 << r.below(8);
                }
                3 => {
                    let p = r.below(d.len() as u64 - 2) as usize;
                    d[p] = r.next() as u8;
                }
                _ => {}
            }
            out.push(d);
        }
        out
    }

    pub(crate) fn run_new(d: &[u8]) -> Outcome {
        match Dct12::parse(d, true) {
            Err(e) => Outcome { parse: Err(e.kind()), blocks: Vec::new(), error: None },
            Ok(t) => {
                let mut blocks = Vec::new();
                let res = t.decode_blocks(d, |y, x, b| blocks.push((y, x, *b)));
                Outcome { parse: Ok((t.width, t.height, t.precision)), blocks, error: res.err().map(|e| e.kind()) }
            }
        }
    }

    pub(crate) fn digest(run: impl Fn(&[u8]) -> Outcome) -> u64 {
        let mut h = 0xcbf2_9ce4_8422_2325u64;
        for s in streams() {
            let o = run(&s);
            match o.parse {
                Ok((w, hh, p)) => {
                    for v in [w, hh, p] {
                        fnv(&mut h, &v.to_le_bytes());
                    }
                }
                Err(k) => fnv(&mut h, k.as_bytes()),
            }
            for (y, x, b) in &o.blocks {
                fnv(&mut h, &y.to_le_bytes());
                fnv(&mut h, &x.to_le_bytes());
                for v in b {
                    fnv(&mut h, &v.to_le_bytes());
                }
            }
            fnv(&mut h, o.error.unwrap_or("ok").as_bytes());
        }
        h
    }

    /// Digest recorded from the previous implementation.
    const GOLDEN_DIGEST: u64 = 0x4aceb34ae4da85c6;

    #[test]
    fn golden_digest_of_generated_tiles() {
        let d = digest(run_new);
        println!("GOLDEN {d:#018x}");
        if GOLDEN_DIGEST != 0 {
            assert_eq!(d, GOLDEN_DIGEST);
        }
    }
}
