//! Golden digests of `deflate`, `jpeg` and `jxl` on synthetic, mutated and real input.
//!
//! Every digest below was recorded from a differential run in which the previous implementation of these three
//! modules and the current one were given the same inputs and agreed on every output: values (bit patterns for
//! floats, NaNs included), the untouched tails of output buffers, decoded pixels, and the kind and message of every
//! error. The one intended difference, `decode_float_row` with zero samples and a non-zero distance (the previous
//! code could panic there; it is now a no-op), is left out of the digests and checked on its own.
//!
//! The lossy DNG in `npm/tests/fixtures` is part of the repository. Parts that need corpus files (`FLASHRAW_CORPUS`,
//! default /var/projects/flashraw-realworld/corpus) are skipped when the files are absent.

use flashraw_codecs::{deflate, jpeg};
use flashraw_core::Limits;

/// Input generators and the digest of each module's observable behaviour, written against plain closures so that
/// any implementation of the module can be measured with them.
#[allow(dead_code)]
pub mod digest {
    use flashraw_core::{Limits, Result};

    pub struct Rng(pub u64);
    impl Rng {
        pub fn step(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        pub fn below(&mut self, n: usize) -> usize {
            if n == 0 { 0 } else { (self.step() % n as u64) as usize }
        }
        pub fn bytes(&mut self, n: usize) -> Vec<u8> {
            (0..n).map(|_| self.step() as u8).collect()
        }
        pub fn bytes_upto(&mut self, max: usize) -> Vec<u8> {
            let n = self.below(max);
            self.bytes(n)
        }
    }

    /// FNV-1a over 64-bit words (one step per word, which keeps the all-values loops quick in debug builds).
    pub struct Fnv(pub u64);
    impl Fnv {
        pub fn new() -> Self {
            Fnv(0xcbf2_9ce4_8422_2325)
        }
    }
    impl Default for Fnv {
        fn default() -> Self {
            Self::new()
        }
    }
    impl Fnv {
        pub fn word(&mut self, v: u64) {
            self.0 = (self.0 ^ v).wrapping_mul(0x100_0000_01b3);
        }
        pub fn bytes(&mut self, b: &[u8]) {
            self.word(b.len() as u64);
            for c in b.chunks(8) {
                let mut w = [0u8; 8];
                w[..c.len()].copy_from_slice(c);
                self.word(u64::from_le_bytes(w));
            }
        }
        pub fn f32s(&mut self, v: &[f32]) {
            self.word(v.len() as u64);
            v.iter().for_each(|x| self.word(u64::from(x.to_bits())));
        }
        pub fn u16s(&mut self, v: &[u16]) {
            self.word(v.len() as u64);
            v.iter().for_each(|&x| self.word(u64::from(x)));
        }
        /// Error identity (kind and message) or, on success, whatever `ok` hashes.
        pub fn result<T>(&mut self, r: &Result<T>, ok: impl FnOnce(&mut Self, &T)) {
            match r {
                Ok(v) => {
                    self.word(0);
                    ok(self, v);
                }
                Err(e) => {
                    self.word(1);
                    self.bytes(e.kind().as_bytes());
                    self.bytes(e.to_string().as_bytes());
                }
            }
        }
    }

    pub fn corpus_file(rel: &str) -> Option<Vec<u8>> {
        let root: std::path::PathBuf = std::env::var_os("FLASHRAW_CORPUS")
            .map(Into::into)
            .unwrap_or_else(|| "/var/projects/flashraw-realworld/corpus".into());
        let d = std::fs::read(root.join(rel)).ok();
        if d.is_none() {
            eprintln!("skipped (absent): {rel}");
        }
        d
    }

    // ------------------------------------------------------------------------------------------- deflate

    /// `f16_to_f32` on all 65,536 patterns, `f24_to_f32` on all 2^24 patterns and on words with high bits set,
    /// `float_bytes_to_f32` on random lengths and widths (output pre-filled with a sentinel NaN).
    pub fn float_helpers(
        f16: impl Fn(u16) -> f32,
        f24: impl Fn(u32) -> f32,
        bytes: impl Fn(&[u8], usize, &mut [f32]),
    ) -> u64 {
        let mut h = Fnv::new();
        for x in 0..=u16::MAX {
            h.word(u64::from(f16(x).to_bits()));
        }
        for x in 0..(1u32 << 24) {
            h.word(u64::from(f24(x).to_bits()));
        }
        let mut r = Rng(7);
        for _ in 0..100_000 {
            h.word(u64::from(f24(r.step() as u32).to_bits()));
        }
        for _ in 0..30_000 {
            let src = r.bytes_upto(80);
            let bps = r.below(7);
            let mut out = vec![f32::from_bits(0x7fc0_1234); r.below(40)];
            bytes(&src, bps, &mut out);
            h.f32s(&out);
        }
        h.0
    }

    /// Integer and floating-point predictors on random rows, including degenerate distances, short rows (no-op),
    /// long rows (tail untouched), short outputs, and rows encoded the way a writer does.
    pub fn predictors(
        horizontal: impl Fn(&mut [u16], usize, usize),
        float_predictor: impl Fn(&mut [u8], &mut Vec<u8>, usize, usize, usize),
        float_row: impl Fn(&mut [u8], usize, usize, usize, bool, &mut [f32]),
    ) -> u64 {
        let mut h = Fnv::new();
        let mut r = Rng(13);
        for _ in 0..20_000 {
            let len = r.below(300);
            let mut row: Vec<u16> = (0..len).map(|_| r.step() as u16).collect();
            horizontal(&mut row, r.below(6), r.below(6));
            h.u16s(&row);
        }
        for _ in 0..20_000 {
            let (width, bps, factor) = (r.below(100), r.below(7), r.below(11));
            let n = width * bps;
            let len = match r.below(4) {
                0 => r.below(n + 1),
                1 => n + r.below(9),
                _ => n,
            };
            let mut row = r.bytes(len);
            let mut tmp = r.bytes_upto(20);
            float_predictor(&mut row, &mut tmp, width, bps, factor);
            h.bytes(&row);
        }
        for _ in 0..30_000 {
            let samples = 1 + r.below(199);
            let bps = r.below(7);
            let factor = if r.below(3) == 0 { 0 } else { r.below(13) };
            let le = r.below(2) == 1;
            let n = samples * bps;
            let len = match r.below(5) {
                0 => r.below(n + 1),
                1 => n + r.below(9),
                _ => n,
            };
            let olen = match r.below(5) {
                0 => r.below(samples + 1),
                1 => samples + r.below(5),
                _ => samples,
            };
            let mut row = r.bytes(len);
            let mut out = vec![f32::from_bits(0x7fc0_4321); olen];
            float_row(&mut row, samples, bps, factor, le, &mut out);
            h.f32s(&out);
        }
        for _ in 0..1_000 {
            let cpp = 1 + r.below(4);
            let samples = (1 + r.below(300)) * cpp;
            let bps = 2 + r.below(3);
            let factor = cpp * [1, 2, 4][r.below(3)];
            let be = r.bytes(samples * bps);
            let mut planes = vec![0u8; be.len()];
            for s in 0..samples {
                for k in 0..bps {
                    planes[k * samples + s] = be[s * bps + k];
                }
            }
            for i in (factor..planes.len()).rev() {
                planes[i] = planes[i].wrapping_sub(planes[i - factor]);
            }
            let mut out = vec![0f32; samples];
            float_row(&mut planes, samples, bps, factor, r.below(2) == 1, &mut out);
            h.f32s(&out);
        }
        h.0
    }

    pub fn zlib(data: &[u8], level: i32) -> Vec<u8> {
        let mut buf = vec![0u8; zlib_rs::compress_bound(data.len())];
        let (c, rc) = zlib_rs::compress_slice(&mut buf, data, zlib_rs::DeflateConfig::new(level));
        assert_eq!(rc, zlib_rs::ReturnCode::Ok);
        c.to_vec()
    }

    fn mutate(r: &mut Rng, z: &[u8]) -> Vec<u8> {
        let mut m = z.to_vec();
        match r.below(6) {
            0 => m.truncate(r.below(z.len() + 1)),
            1 if !m.is_empty() => {
                let i = r.below(m.len());
                m[i] ^= 1 << r.below(8);
            }
            2 => {
                let k = 1 + r.below(8);
                m.extend(r.bytes(k));
            }
            3 if m.len() > 4 => {
                let l = m.len();
                m[l - 1 - r.below(4)] ^= 0x55;
            }
            4 if m.len() > 1 => m[r.below(2)] ^= 1 << r.below(8),
            _ => {}
        }
        m
    }

    /// Both inflate entry points on valid and damaged zlib streams with every kind of ceiling and buffer size
    /// (buffers pre-filled, so untouched tails are part of the digest), plus short garbage.
    pub fn inflate(
        to_vec: impl Fn(&[u8], usize) -> Result<Vec<u8>>,
        into: impl Fn(&[u8], &mut [u8]) -> Result<usize>,
    ) -> u64 {
        let mut h = Fnv::new();
        let mut r = Rng(29);
        for case in 0..400 {
            let n = [0, 1, 5, 100, 4096, 65_537][r.below(6)] + r.below(50);
            let payload: Vec<u8> = match r.below(3) {
                0 => r.bytes(n),
                1 => (0..n).map(|i| (i / 7) as u8).collect(),
                _ => (0..n).map(|_| if r.below(8) == 0 { r.step() as u8 } else { 0 }).collect(),
            };
            let z = zlib(&payload, [0, 1, 6, 9][r.below(4)]);
            let z = if case % 3 == 0 { z } else { mutate(&mut r, &z) };
            for max in [n, n + 1 + r.below(100), n.saturating_sub(1 + r.below(10)), 0, usize::MAX] {
                h.result(&to_vec(&z, max), |h, v| h.bytes(v));
            }
            for cap in [n, n + 1 + r.below(100), n.saturating_sub(1 + r.below(10)), 0] {
                let mut out = vec![0xA5u8; cap];
                let got = into(&z, &mut out);
                h.result(&got, |h, &len| {
                    h.word(len as u64);
                    h.bytes(&out);
                });
            }
        }
        for _ in 0..5_000 {
            let z = r.bytes_upto(24);
            h.result(&to_vec(&z, 1 << 16), |h, v| h.bytes(v));
            h.result(&into(&z, &mut [0u8; 64]), |h, &len| h.word(len as u64));
        }
        h.0
    }

    /// Compression, bits per sample and the byte ranges of the strips or tiles of one IFD.
    pub type Unit = (u32, u32, Vec<(usize, usize)>);

    /// (compression, bits per sample, byte ranges of strips or tiles) of every IFD reachable through IFD chains and
    /// SubIFDs of a TIFF file.
    pub fn tiff_units(d: &[u8]) -> Vec<Unit> {
        let le = match d.get(0..2) {
            Some(b"II") => true,
            Some(b"MM") => false,
            _ => return vec![],
        };
        let u16_at = |p: usize| {
            d.get(p..p + 2)
                .map(|b| if le { u16::from_le_bytes([b[0], b[1]]) } else { u16::from_be_bytes([b[0], b[1]]) })
        };
        let u32_at = |p: usize| {
            d.get(p..p + 4).map(|b| {
                let a = [b[0], b[1], b[2], b[3]];
                if le { u32::from_le_bytes(a) } else { u32::from_be_bytes(a) }
            })
        };
        let values = |e: usize| -> Vec<u32> {
            let (Some(t), Some(c)) = (u16_at(e + 2), u32_at(e + 4)) else { return vec![] };
            let size = match t {
                3 => 2,
                4 | 13 => 4,
                _ => return vec![],
            };
            let c = c.min(100_000) as usize;
            let base = if size * c <= 4 { e + 8 } else { u32_at(e + 8).unwrap_or(u32::MAX) as usize };
            (0..c)
                .filter_map(|i| if size == 2 { u16_at(base + 2 * i).map(u32::from) } else { u32_at(base + 4 * i) })
                .collect()
        };
        let mut out = vec![];
        let mut todo = vec![u32_at(4).unwrap_or(0) as usize];
        let mut seen = std::collections::HashSet::new();
        while let Some(mut o) = todo.pop() {
            while o != 0 && seen.insert(o) && seen.len() < 64 {
                let Some(n) = u16_at(o) else { break };
                let mut tags = std::collections::HashMap::new();
                for i in 0..n as usize {
                    let e = o + 2 + 12 * i;
                    if let Some(tag) = u16_at(e) {
                        tags.insert(tag, values(e));
                    }
                }
                if let Some(subs) = tags.get(&330) {
                    todo.extend(subs.iter().map(|&s| s as usize));
                }
                let first = |t: u16| tags.get(&t).and_then(|v| v.first().copied());
                if let Some(comp) = first(259) {
                    let (offs, lens) = match (tags.get(&324), tags.get(&325), tags.get(&273), tags.get(&279)) {
                        (Some(a), Some(b), _, _) | (_, _, Some(a), Some(b)) => (a.clone(), b.clone()),
                        _ => (vec![], vec![]),
                    };
                    let ranges = offs
                        .iter()
                        .zip(&lens)
                        .map(|(&a, &b)| (a as usize, (a as usize).saturating_add(b as usize).min(d.len())))
                        .filter(|(a, b)| a < b)
                        .collect();
                    out.push((comp, first(258).unwrap_or(1), ranges));
                }
                o = u32_at(o + 2 + 12 * n as usize).unwrap_or(0) as usize;
            }
        }
        out
    }

    /// The first tiles of the three deflate HDR-merge DNGs: inflated both ways with exact, larger and smaller
    /// buffers and when cut short, and every row decoded with the files' predictor distance (2). `None` when the
    /// files are absent.
    pub fn hdrmerge_tiles(
        to_vec: impl Fn(&[u8], usize) -> Result<Vec<u8>>,
        into: impl Fn(&[u8], &mut [u8]) -> Result<usize>,
        float_row: impl Fn(&mut [u8], usize, usize, usize, bool, &mut [f32]),
    ) -> Option<u64> {
        let mut h = Fnv::new();
        for (rel, bps, width) in [
            ("Canon/EOS 5D Mark III/hdrmerge-bayer-fp16-w-pred-deflate.dng", 2, 496),
            ("Canon/EOS 5D Mark III/hdrmerge-bayer-fp24-w-pred-deflate.dng", 3, 400),
            ("Canon/EOS 5D Mark III/hdrmerge-bayer-fp32-w-pred-deflate.dng", 4, 400),
        ] {
            let d = corpus_file(rel)?;
            for (comp, _, ranges) in tiff_units(&d) {
                if comp != 8 {
                    continue;
                }
                for &(a, b) in ranges.iter().take(6) {
                    let z = &d[a..b];
                    let whole = to_vec(z, 1 << 26);
                    h.result(&whole, |h, v| h.bytes(v));
                    let Ok(whole) = whole else { continue };
                    for cap in [whole.len(), whole.len() + 7, whole.len() - 1] {
                        let mut out = vec![1u8; cap];
                        let got = into(z, &mut out);
                        h.result(&got, |h, &len| {
                            h.word(len as u64);
                            h.bytes(&out);
                        });
                    }
                    for cut in [z.len() / 2, z.len() - 1] {
                        h.result(&to_vec(&z[..cut], 1 << 26), |h, v| h.bytes(v));
                    }
                    let mut out = vec![0f32; width];
                    for row in whole.chunks_exact(width * bps) {
                        float_row(&mut row.to_vec(), width, bps, 2, false, &mut out);
                        h.f32s(&out);
                    }
                }
            }
        }
        Some(h.0)
    }

    // ------------------------------------------------------------------------------------------- jpeg

    /// `dimensions` on random marker sequences (every quirk: fill bytes, markers with and without a length, EOI
    /// and stuffed zeros read with a length, scans, frames with length 0) and on cuts of them.
    pub fn jpeg_markers(dims: impl Fn(&[u8]) -> Option<(u32, u32)>) -> u64 {
        let tokens: [&[u8]; 14] = [
            &[0xFF],
            &[0xFF, 0xFF],
            &[0xFF, 0xD8],
            &[0xFF, 0xD9],
            &[0xFF, 0x01],
            &[0xFF, 0xD3],
            &[0xFF, 0x00],
            &[0xFF, 0xDA, 0x00, 0x02],
            &[0xFF, 0xE1, 0x00, 0x04, 1, 2],
            &[0xFF, 0xC4, 0x00, 0x03, 9],
            &[0xFF, 0xC0, 0x00, 0x11, 8, 0x0F, 0xA0, 0x17, 0x70, 3],
            &[0xFF, 0xC2, 0x00, 0x00, 8, 0, 1, 0, 2],
            &[0xFF, 0xCC, 0x00, 0x05, 1, 2, 3],
            &[0xFF, 0xC8, 0x00, 0x02],
        ];
        let mut h = Fnv::new();
        let put = |h: &mut Fnv, v: Option<(u32, u32)>| match v {
            None => h.word(u64::MAX),
            Some((w, ht)) => h.word(u64::from(w) << 32 | u64::from(ht)),
        };
        let mut r = Rng(31);
        for _ in 0..100_000 {
            let mut d = if r.below(8) == 0 { vec![] } else { vec![0xFF, 0xD8] };
            for _ in 0..r.below(8) {
                if r.below(3) == 0 {
                    d.extend(r.bytes_upto(6));
                } else {
                    d.extend_from_slice(tokens[r.below(tokens.len())]);
                }
                if r.below(4) == 0 {
                    let m = 0xC0 + r.below(0x40) as u8;
                    d.extend_from_slice(&[0xFF, m, 0, r.below(20) as u8]);
                    d.extend(r.bytes_upto(12));
                }
            }
            put(&mut h, dims(&d));
            for cut in [d.len().saturating_sub(1), d.len() / 2] {
                put(&mut h, dims(&d[..cut]));
            }
        }
        h.0
    }

    /// MSB-first bit writer with JPEG byte stuffing (0xFF is followed by 0x00).
    struct Bits {
        acc: u64,
        count: u32,
        out: Vec<u8>,
    }
    impl Bits {
        fn put(&mut self, value: u32, len: u32) {
            self.acc = self.acc << len | u64::from(value & ((1u32 << len) - 1));
            self.count += len;
            while self.count >= 8 {
                self.count -= 8;
                let b = (self.acc >> self.count) as u8;
                self.out.push(b);
                if b == 0xFF {
                    self.out.push(0);
                }
            }
        }
    }

    /// A baseline JPEG of `components` (1 or 3) components whose 8×8 blocks are flat with random levels: quantiser
    /// all ones, DC coded with the T.81 K.3 luminance DC table, AC with a single one-bit code for end-of-block.
    pub fn flat_jpeg(width: u16, height: u16, components: u8, r: &mut Rng) -> Vec<u8> {
        const DC_COUNTS: [u8; 16] = [0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0];
        let mut o = vec![0xFF, 0xD8, 0xFF, 0xDB, 0, 67, 0];
        o.extend([1u8; 64]);
        o.extend([0xFF, 0xC0]);
        o.extend((8 + 3 * u16::from(components)).to_be_bytes());
        o.push(8);
        o.extend(height.to_be_bytes());
        o.extend(width.to_be_bytes());
        o.push(components);
        for c in 0..components {
            o.extend([c + 1, 0x11, 0]);
        }
        o.extend([0xFF, 0xC4, 0, 31, 0x00]);
        o.extend(DC_COUNTS);
        o.extend(0..12u8);
        o.extend([0xFF, 0xC4, 0, 20, 0x10, 1]);
        o.extend([0u8; 15]);
        o.push(0x00);
        o.extend([0xFF, 0xDA]);
        o.extend((6 + 2 * u16::from(components)).to_be_bytes());
        o.push(components);
        for c in 0..components {
            o.extend([c + 1, 0x00]);
        }
        o.extend([0, 63, 0]);
        // Canonical DC codes (T.81 Annex C): (code, length) per category.
        let mut dc_codes = [(0u32, 0u32); 12];
        let (mut code, mut sym) = (0u32, 0usize);
        for (len, &count) in DC_COUNTS.iter().enumerate() {
            for _ in 0..count {
                dc_codes[sym] = (code, len as u32 + 1);
                code += 1;
                sym += 1;
            }
            code <<= 1;
        }
        let mut bits = Bits { acc: 0, count: 0, out: Vec::new() };
        let blocks = usize::from(width).div_ceil(8) * usize::from(height).div_ceil(8);
        let mut pred = [0i32; 3];
        for _ in 0..blocks {
            for p in pred.iter_mut().take(usize::from(components)) {
                let dc = 8 * (r.below(256) as i32 - 128);
                let diff = dc - *p;
                *p = dc;
                let cat = 32 - diff.unsigned_abs().leading_zeros();
                let (code, len) = dc_codes[cat as usize];
                bits.put(code, len);
                if cat > 0 {
                    let extra = if diff > 0 { diff } else { diff + (1 << cat) - 1 };
                    bits.put(extra as u32, cat);
                }
                bits.put(0, 1); // end of block
            }
        }
        let pad = (8 - bits.count % 8) % 8;
        if pad > 0 {
            bits.put(0xFF, pad);
        }
        o.extend(bits.out);
        o.extend([0xFF, 0xD9]);
        o
    }

    /// `decode` (and `dimensions`) on synthetic grey and colour JPEGs, the lossy tiles and previews of the in-repo
    /// DNG fixture, and truncated or byte-flipped copies, under default and tight limits.
    pub fn jpeg_streams(
        dims: impl Fn(&[u8]) -> Option<(u32, u32)>,
        decode: impl Fn(&[u8], &Limits) -> Result<(usize, usize, usize, Vec<u8>)>,
    ) -> u64 {
        let limit_sets =
            [Limits::default(), Limits::default().with_max_dimension(160), Limits::default().with_max_samples(30_000)];
        let mut h = Fnv::new();
        let mut r = Rng(37);
        let check = |h: &mut Fnv, s: &[u8], limits: &Limits| {
            h.result(&decode(s, limits), |h, (w, ht, c, data)| {
                h.word(*w as u64);
                h.word(*ht as u64);
                h.word(*c as u64);
                h.bytes(data);
            });
        };
        let mut streams = vec![];
        for (w, ht) in [(8, 8), (16, 16), (20, 13), (171, 97), (200, 160)] {
            for comps in [1, 3] {
                streams.push(flat_jpeg(w, ht, comps, &mut r));
            }
        }
        let fixture =
            std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../npm/tests/fixtures/canon-5d3-lossy.dng"))
                .expect("lossy DNG fixture");
        let mut at = 0;
        while let Some(k) = fixture[at..].windows(3).position(|w| w == [0xFF, 0xD8, 0xFF]) {
            streams.push(fixture[at + k..].to_vec());
            at += k + 1;
        }
        for (i, s) in streams.iter().enumerate() {
            let size = dims(s);
            h.word(size.map_or(u64::MAX, |(w, ht)| u64::from(w) << 32 | u64::from(ht)));
            if size.is_none_or(|(w, ht)| u64::from(w) * u64::from(ht) > 1_500_000) {
                continue;
            }
            for l in &limit_sets {
                check(&mut h, s, l);
            }
            if i % 3 == 0 {
                let end = s.len().min(1 << 20);
                let cut = r.below(end).max(2);
                check(&mut h, &s[..cut], &Limits::default());
                let mut m = s[..end].to_vec();
                let j = 2 + r.below(m.len().min(2048) - 2);
                m[j] ^= 1 << r.below(8);
                check(&mut h, &m, &Limits::default());
            }
        }
        h.0
    }

    // ------------------------------------------------------------------------------------------- jxl

    pub type JxlResult = Result<(usize, usize, usize, Vec<u16>)>;

    fn hash_jxl(h: &mut Fnv, r: &JxlResult) {
        h.result(r, |h, (w, ht, c, data)| {
            h.word(*w as u64);
            h.word(*ht as u64);
            h.word(*c as u64);
            h.u16s(data);
        });
    }

    /// Short random inputs, half of them with the bare-codestream signature: error kind and message.
    pub fn jxl_garbage(decode: impl Fn(&[u8], u32) -> JxlResult) -> u64 {
        let mut h = Fnv::new();
        let mut r = Rng(41);
        for _ in 0..2_000 {
            let mut s = r.bytes_upto(64);
            if r.below(2) == 0 && s.len() >= 2 {
                s[0] = 0xFF;
                s[1] = 0x0A;
            }
            hash_jxl(&mut h, &decode(&s, 16));
        }
        h.0
    }

    /// The JPEG XL strip of the Galaxy S22 DNG at several container depths, and cut short. `None` when absent.
    pub fn jxl_file(decode: impl Fn(&[u8], u32) -> JxlResult) -> Option<u64> {
        let d = corpus_file("samsung/SM-S901E/20250305_173647.dng")?;
        let mut h = Fnv::new();
        for (comp, bps, ranges) in tiff_units(&d) {
            if comp != 52546 {
                continue;
            }
            for &(a, b) in ranges.iter().take(2) {
                let s = &d[a..b];
                for bits in [bps, 12, 0, 32] {
                    hash_jxl(&mut h, &decode(s, bits));
                }
                for cut in [s.len() / 3, s.len() - 1, 16, 0] {
                    hash_jxl(&mut h, &decode(&s[..cut], bps));
                }
            }
        }
        Some(h.0)
    }
}

fn jpeg_decode(s: &[u8], limits: &Limits) -> flashraw_core::Result<(usize, usize, usize, Vec<u8>)> {
    jpeg::decode(s, limits).map(|d| (d.width, d.height, d.components, d.data))
}

#[cfg(feature = "jpegxl")]
fn jxl_decode(s: &[u8], bits: u32) -> digest::JxlResult {
    flashraw_codecs::jxl::decode(s, bits).map(|d| (d.width, d.height, d.channels, d.data))
}

#[test]
fn float_helpers() {
    let got = digest::float_helpers(deflate::f16_to_f32, deflate::f24_to_f32, deflate::float_bytes_to_f32);
    assert_eq!(got, GOLDEN_FLOAT_HELPERS, "{got:#018x}");
}

#[test]
fn predictors() {
    let got =
        digest::predictors(deflate::undo_horizontal_u16, deflate::undo_float_predictor, deflate::decode_float_row);
    assert_eq!(got, GOLDEN_PREDICTORS, "{got:#018x}");
}

/// The approved change: zero samples is a no-op for every distance and width.
#[test]
fn float_row_without_samples_does_nothing() {
    for bps in 0..6 {
        for factor in 0..6 {
            for len in [0, 3] {
                let mut row = vec![7u8; len];
                let mut out = [f32::from_bits(0x7fc0_4321); 2];
                deflate::decode_float_row(&mut row, 0, bps, factor, false, &mut out);
                assert_eq!(out.map(f32::to_bits), [0x7fc0_4321; 2]);
            }
        }
    }
}

/// The digest covers streams the test compresses itself, and the compressor's output differs
/// between CPU architectures, so the pinned value holds on x86-64 only; the round trip below
/// holds everywhere.
#[test]
fn inflate() {
    let got = digest::inflate(deflate::inflate_zlib, deflate::inflate_zlib_into);
    if cfg!(target_arch = "x86_64") {
        assert_eq!(got, GOLDEN_INFLATE, "{got:#018x}");
    }
}

#[test]
fn inflate_round_trip() {
    for n in [0usize, 1, 5, 100, 4096, 65_537] {
        let payload: Vec<u8> = (0..n).map(|i| (i * 7 / 3) as u8 ^ (i >> 9) as u8).collect();
        for level in [0, 1, 6, 9] {
            let z = digest::zlib(&payload, level);
            assert_eq!(deflate::inflate_zlib(&z, n).unwrap(), payload);
            let mut out = vec![0xA5u8; n + 3];
            assert_eq!(deflate::inflate_zlib_into(&z, &mut out).unwrap(), n);
            assert_eq!(&out[..n], &payload[..]);
            assert_eq!(&out[n..], &[0xA5; 3]);
        }
    }
}

#[test]
fn hdrmerge_tiles() {
    let got = digest::hdrmerge_tiles(deflate::inflate_zlib, deflate::inflate_zlib_into, deflate::decode_float_row);
    if let Some(got) = got {
        assert_eq!(got, GOLDEN_HDRMERGE_TILES, "{got:#018x}");
    }
}

#[test]
fn jpeg_markers() {
    let got = digest::jpeg_markers(jpeg::dimensions);
    assert_eq!(got, GOLDEN_JPEG_MARKERS, "{got:#018x}");
}

#[test]
fn jpeg_streams() {
    let got = digest::jpeg_streams(jpeg::dimensions, jpeg_decode);
    assert_eq!(got, GOLDEN_JPEG_STREAMS, "{got:#018x}");
}

/// The synthetic streams behind the JPEG digest are valid: grey stays one component, colour becomes RGB, and flat
/// blocks stay flat.
#[test]
fn synthetic_jpegs_decode() {
    for comps in [1u8, 3] {
        let s = digest::flat_jpeg(20, 13, comps, &mut digest::Rng(5));
        assert_eq!(jpeg::dimensions(&s), Some((20, 13)));
        let d = jpeg::decode(&s, &Limits::default()).unwrap();
        assert_eq!((d.width, d.height, d.components), (20, 13, if comps == 1 { 1 } else { 3 }));
        assert_eq!(d.data.len(), 20 * 13 * d.components);
    }
    // Every 8×8 block of the grey image is flat.
    let d = jpeg::decode(&digest::flat_jpeg(32, 16, 1, &mut digest::Rng(9)), &Limits::default()).unwrap();
    for y in 0..16 {
        for x in 0..32 {
            assert_eq!(d.data[y * 32 + x], d.data[(y / 8 * 8) * 32 + x / 8 * 8], "({x}, {y})");
        }
    }
}

#[cfg(feature = "jpegxl")]
#[test]
fn jxl_garbage() {
    let got = digest::jxl_garbage(jxl_decode);
    assert_eq!(got, GOLDEN_JXL_GARBAGE, "{got:#018x}");
}

#[cfg(feature = "jpegxl")]
#[test]
fn jxl_file() {
    if let Some(got) = digest::jxl_file(jxl_decode) {
        assert_eq!(got, GOLDEN_JXL_FILE, "{got:#018x}");
    }
}

const GOLDEN_FLOAT_HELPERS: u64 = 0x4423_a282_1ddc_ef59;
const GOLDEN_PREDICTORS: u64 = 0x776d_185c_8cd0_5221;
const GOLDEN_INFLATE: u64 = 0x5a2b_97be_eacb_9e6e;
const GOLDEN_HDRMERGE_TILES: u64 = 0x559c_4db5_1e41_d7bd;
const GOLDEN_JPEG_MARKERS: u64 = 0x6029_6dfb_0ba2_a024;
const GOLDEN_JPEG_STREAMS: u64 = 0xcc7e_376c_79e2_14d6;
#[cfg(feature = "jpegxl")]
const GOLDEN_JXL_GARBAGE: u64 = 0xc989_567f_388b_7283;
#[cfg(feature = "jpegxl")]
const GOLDEN_JXL_FILE: u64 = 0xb01c_88bb_28bb_75da;
