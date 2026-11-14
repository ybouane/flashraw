//! Deterministic fixtures for the Samsung decoders: pseudo-random V0/V1 streams, a V2 stream
//! generator that follows the block syntax, and a digest of what a decoder makes of them.

use flashraw_core::Result;

pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    /// `n` bytes; `bias` 0 uniform, 1 mostly zero bits, 2 mostly one bits, 3 small values.
    pub fn bytes(&mut self, n: usize, bias: usize) -> Vec<u8> {
        (0..n)
            .map(|_| {
                let b = self.next() as u8;
                match bias {
                    0 => b,
                    1 => b & self.next() as u8,
                    2 => b | self.next() as u8,
                    _ => b & 0x0F,
                }
            })
            .collect()
    }
}

/// Non-zero background pattern: proves which samples a decoder leaves alone.
pub fn background(n: usize, seed: u64) -> Vec<u16> {
    let mut r = Rng(seed | 1);
    (0..n).map(|_| (r.next() >> 20) as u16 | 1).collect()
}

fn fnv(out: &[u16]) -> u64 {
    out.iter()
        .flat_map(|v| v.to_le_bytes())
        .fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3))
}

/// Writer for the word bit stream (little-endian words, most significant bit first).
pub struct BitW {
    words: Vec<u32>,
    cur: u32,
    nb: u32,
}

impl BitW {
    pub fn new() -> Self {
        BitW { words: vec![], cur: 0, nb: 0 }
    }
    pub fn put(&mut self, v: u32, n: u32) {
        for i in (0..n).rev() {
            self.cur = (self.cur << 1) | ((v >> i) & 1);
            self.nb += 1;
            if self.nb == 32 {
                self.words.push(self.cur);
                self.cur = 0;
                self.nb = 0;
            }
        }
    }
    /// End the current row: pad to a whole word, then to a multiple of 16 bytes.
    pub fn end_row(&mut self) {
        if self.nb > 0 {
            self.words.push(self.cur << (32 - self.nb));
            self.cur = 0;
            self.nb = 0;
        }
        while self.words.len() % 4 != 0 {
            self.words.push(0);
        }
    }
    pub fn bytes(&self) -> Vec<u8> {
        self.words.iter().flat_map(|w| w.to_le_bytes()).collect()
    }
}

/// A V2 stream of `w x h` samples. `bad` lets an occasional block break the rules (a motion
/// selector on the first rows, an impossible bit length).
pub fn gen_v2(r: &mut Rng, w: usize, h: usize, depth: u32, opt: u32, init: u32, bad: bool) -> Vec<u8> {
    let mut b = BitW::new();
    for (v, n) in
        [(0xAB, 16), (0, 4), (depth - 1, 4), (0, 8), (w as u32, 16), (h as u32, 16), (0, 16), (0, 4), (opt, 4)]
    {
        b.put(v, n);
    }
    for (v, n) in [(0, 16), (0, 8), (0, 2), (init, 14)] {
        b.put(v, n);
    }
    b.end_row();
    let top = depth as i32 + 1;
    for row in 0..h {
        let start = if row < 2 { 7i32 } else { 4 };
        let mut hist = [(start, start); 3];
        let mut motion = 7u32;
        for col in (0..w - 15).step_by(16) {
            if opt & 4 == 0 && col % 64 == 0 {
                let op = r.below(4) as u32;
                b.put(op, 2);
                if op == 3 {
                    b.put(if r.below(2) == 0 { r.below(40) as u32 } else { r.below(4096) as u32 }, 12);
                }
            }
            let mut want =
                if row < 2 && !(bad && r.below(300) == 0) || r.below(2) == 0 { 7 } else { r.below(7) as u32 };
            if row == 2 && col < 4 && want < 3 && !bad {
                want = 3; // keep the reference inside the buffer
            }
            if opt & 2 != 0 {
                motion = if want == 7 { 7 } else { 3 };
                b.put((motion == 3) as u32, 1);
            } else if want == motion {
                b.put(1, 1);
            } else {
                b.put(0, 1);
                b.put(want, 3);
                motion = want;
            }
            let mut dl = [0i32; 4];
            let present = r.below(8) != 0;
            if opt & 1 == 0 {
                b.put(!present as u32, 1);
            }
            if present || opt & 1 != 0 {
                let mut flags = [0u32; 4];
                let mut vals = [0i32; 4];
                let mut h2 = hist;
                for i in 0..4 {
                    let k = if row % 2 == 1 { i / 2 } else { (i / 2 + 2) % 3 };
                    let (a, older) = h2[k];
                    let cand = |f: u32| match f {
                        0 => a,
                        1 => a + 1,
                        2 => a - 1,
                        _ => -1,
                    };
                    let mut f = r.below(4) as u32;
                    let mut tries = 0;
                    while f != 3 && !(0..=top).contains(&cand(f)) && !(bad && r.below(500) == 0) && tries < 8 {
                        f = r.below(4) as u32;
                        tries += 1;
                    }
                    let v = if f == 3 {
                        if r.below(3) == 0 { 0 } else { r.below(top.min(15) as usize + 1) as i32 }
                    } else {
                        cand(f)
                    };
                    flags[i] = f;
                    vals[i] = v;
                    h2[k] = (older, v);
                }
                for f in flags {
                    b.put(f, 2);
                }
                for i in 0..4 {
                    if flags[i] == 3 {
                        b.put(vals[i] as u32 & 15, 4);
                    }
                }
                hist = h2;
                dl = vals;
            }
            for n in dl {
                if (1..=32).contains(&n) {
                    for _ in 0..4 {
                        b.put(r.next() as u32 & (u32::MAX >> (32 - n)), n as u32);
                    }
                }
            }
        }
        b.end_row();
    }
    b.bytes()
}

pub type V0Fn = fn(&[u8], &[u32], usize, usize, &mut [u16], bool) -> Result<()>;
pub type V1Fn = fn(&[u8], usize, usize, &mut [u16]) -> Result<Option<usize>>;
pub type V2Fn = fn(&[u8], usize, usize, &mut [u16]) -> Result<()>;

/// The decoders under test.
pub struct Decoders {
    pub v0: V0Fn,
    pub v1: V1Fn,
    pub v2: V2Fn,
}

fn outcome<T: std::fmt::Debug>(r: &Result<T>, out: &[u16]) -> String {
    match r {
        Ok(v) => format!("ok {v:?} {:016x}", fnv(out)),
        Err(e) => format!("{} {e:?}", e.kind()),
    }
}

/// One line per fixture: what `d` makes of it. Compared with [`GOLDEN`].
pub fn digests(d: &Decoders) -> Vec<String> {
    let mut lines = Vec::new();
    // V0: spill widths, narrow images, offsets inside, beyond and shared.
    let mut r = Rng(0x5eed_0001);
    let widths = [1usize, 2, 3, 7, 15, 16, 17, 26, 31, 32, 42, 58, 64, 90, 96, 106];
    for (i, &w) in widths.iter().enumerate() {
        let h = 1 + (i * 5) % 11;
        let len = 40 + w * h * 2;
        let bias = i % 4;
        let src = r.bytes(len, bias);
        let offs: Vec<u32> = (0..h)
            .map(|k| match i % 4 {
                0 => (k * len / h) as u32,
                1 => 0,
                2 => r.below(len + 16) as u32,
                _ => (len + k) as u32,
            })
            .collect();
        for par in [false, true] {
            let mut out = background(w * h, i as u64);
            let res = (d.v0)(&src, &offs, w, h, &mut out, par);
            lines.push(format!("v0 {w}x{h} par={par} {}", outcome(&res, &out)));
        }
    }
    // V0: several 512-column blocks and several row bands.
    let (w, h) = (1100usize, 40usize);
    let src = r.bytes(w * h * 2, 3);
    let offs: Vec<u32> = (0..h).map(|k| (k * w * 2 / 3) as u32).collect();
    let mut out = background(w * h, 7);
    let res = (d.v0)(&src, &offs, w, h, &mut out, true);
    lines.push(format!("v0 {w}x{h} wide {}", outcome(&res, &out)));
    let mut out = vec![0u16; 4];
    lines.push(format!("v0 short-out {:?}", (d.v0)(&src, &offs, w, h, &mut out, false).map_err(|e| e.to_string())));
    // V1.
    for (i, (w, h)) in [(0usize, 4usize), (1, 4), (2, 3), (33, 9), (64, 20), (250, 31)].into_iter().enumerate() {
        let src = r.bytes(w * h / 2 + 8, i % 4);
        let mut out = background(w * h, i as u64);
        let res = (d.v1)(&src, w, h, &mut out);
        lines.push(format!("v1 {w}x{h} {}", outcome(&res, &out)));
    }
    // V2: option combinations, depths, truncation and corruption.
    let mut r = Rng(0x5eed_0002);
    for (i, opt) in [0u32, 5, 1, 2, 4, 3, 6, 7, 8, 13].into_iter().enumerate() {
        let (w, h) = [(646usize, 436usize), (660, 440)][i % 2];
        let depth = [12u32, 14, 8, 16][i % 4];
        let s = gen_v2(&mut r, w, h, depth, opt, if i % 3 == 0 { 128 } else { 9000 }, i % 5 == 4);
        let mut run = |tag: &str, s: &[u8]| {
            let mut out = background(w * h, i as u64);
            let res = (d.v2)(s, w, h, &mut out);
            lines.push(format!("v2 opt={opt} {w}x{h} d={depth} {tag} {}", outcome(&res, &out)));
        };
        run("full", &s);
        run("short1", &s[..s.len() - 1]);
        run("half", &s[..s.len() / 2]);
        run("tiny", &s[..13]);
        let mut m = s.clone();
        m[5] ^= 0x10;
        m[s.len() / 3] ^= 0x04;
        run("flip", &m);
        let mut m = s.clone();
        m[8] ^= 0x01; // option byte
        run("flip-opt", &m);
    }
    for (w, h) in [(645usize, 436usize), (646, 435), (15, 436), (646, 0), (0, 0), (646, 436)] {
        let mut out = vec![0u16; if w == 646 && h == 436 { 5 } else { w * h }];
        let res = (d.v2)(&[0xFF; 64], w, h, &mut out);
        lines.push(format!("v2 geometry {w}x{h} {}", outcome(&res, &out)));
    }
    lines
}

/// Expected output of [`digests`] (computed once with the previous implementation, which the
/// rewrite matches bit for bit including error messages).
pub const GOLDEN: &[&str] = &[
    "v0 1x1 par=false ok () 0b09cc07b7555433",
    "v0 1x1 par=true ok () 0b09cc07b7555433",
    "v0 2x6 par=false ok () e76ef07969e223ed",
    "v0 2x6 par=true ok () e76ef07969e223ed",
    "v0 3x11 par=false ok () b4b16b6179c8d6a5",
    "v0 3x11 par=true ok () b4b16b6179c8d6a5",
    "v0 7x5 par=false ok () 19b2dc2ebb4956f6",
    "v0 7x5 par=true ok () 19b2dc2ebb4956f6",
    "v0 15x10 par=false ok () 99ff6fbd4f4ffcd5",
    "v0 15x10 par=true ok () 99ff6fbd4f4ffcd5",
    "v0 16x4 par=false ok () 5499b4b61f1ffc15",
    "v0 16x4 par=true ok () 5499b4b61f1ffc15",
    "v0 17x9 par=false ok () 1e49309debe93b2c",
    "v0 17x9 par=true ok () 1e49309debe93b2c",
    "v0 26x3 par=false ok () dceb907b8cd01cbf",
    "v0 26x3 par=true ok () dceb907b8cd01cbf",
    "v0 31x8 par=false ok () 3ba95c34d0a0674b",
    "v0 31x8 par=true ok () 3ba95c34d0a0674b",
    "v0 32x2 par=false ok () 64af6f4f9027c9ad",
    "v0 32x2 par=true ok () 64af6f4f9027c9ad",
    "v0 42x7 par=false ok () 9de00b4313b36cf5",
    "v0 42x7 par=true ok () 9de00b4313b36cf5",
    "v0 58x1 par=false ok () 968d7989b4c570a0",
    "v0 58x1 par=true ok () 968d7989b4c570a0",
    "v0 64x6 par=false ok () 4d0792a339c9a953",
    "v0 64x6 par=true ok () 4d0792a339c9a953",
    "v0 90x11 par=false ok () cb911448b2e52299",
    "v0 90x11 par=true ok () cb911448b2e52299",
    "v0 96x5 par=false ok () ecaa2433709f17ca",
    "v0 96x5 par=true ok () ecaa2433709f17ca",
    "v0 106x10 par=false ok () a47ff2a4e5f9c248",
    "v0 106x10 par=true ok () a47ff2a4e5f9c248",
    "v0 1100x40 wide ok () bab7df38d7ac0445",
    "v0 short-out Err(\"invalid argument: Samsung v0: buffer or offsets too small\")",
    "v1 0x4 InvalidArgument InvalidArgument(\"predictive Huffman: invalid width\")",
    "v1 1x4 InvalidArgument InvalidArgument(\"predictive Huffman: invalid width\")",
    "v1 2x3 ok None 7ca9a776e789c7fd",
    "v1 33x9 ok Some(4) cc17f32213c9a199",
    "v1 64x20 ok Some(10) 0d27657bf2a0ca19",
    "v1 250x31 ok Some(16) 9c196a6cd5e4968b",
    "v2 opt=0 646x436 d=12 full ok () 946d29b68fe6b446",
    "v2 opt=0 646x436 d=12 short1 CorruptFile CorruptFile { what: \"Samsung v2: truncated data at row 435/436\", offset: None, reason: None }",
    "v2 opt=0 646x436 d=12 half CorruptFile CorruptFile { what: \"Samsung v2: truncated data at row 217/436\", offset: None, reason: None }",
    "v2 opt=0 646x436 d=12 tiny CorruptFile CorruptFile { what: \"Samsung v2: truncated data at row 0/436\", offset: None, reason: None }",
    "v2 opt=0 646x436 d=12 flip CorruptFile CorruptFile { what: \"Samsung v2: invalid difference length\", offset: None, reason: None }",
    "v2 opt=0 646x436 d=12 flip-opt ok () 946d29b68fe6b446",
    "v2 opt=5 660x440 d=14 full ok () 10839e965c188779",
    "v2 opt=5 660x440 d=14 short1 ok () 10839e965c188779",
    "v2 opt=5 660x440 d=14 half CorruptFile CorruptFile { what: \"Samsung v2: truncated data at row 219/440\", offset: None, reason: None }",
    "v2 opt=5 660x440 d=14 tiny CorruptFile CorruptFile { what: \"Samsung v2: truncated data at row 0/440\", offset: None, reason: None }",
    "v2 opt=5 660x440 d=14 flip ok () 9f54c27d64a857b6",
    "v2 opt=5 660x440 d=14 flip-opt ok () 10839e965c188779",
    "v2 opt=1 646x436 d=8 full ok () f1c88f74990aca05",
    "v2 opt=1 646x436 d=8 short1 ok () f1c88f74990aca05",
    "v2 opt=1 646x436 d=8 half CorruptFile CorruptFile { what: \"Samsung v2: invalid difference length\", offset: None, reason: None }",
    "v2 opt=1 646x436 d=8 tiny CorruptFile CorruptFile { what: \"Samsung v2: invalid difference length\", offset: None, reason: None }",
    "v2 opt=1 646x436 d=8 flip CorruptFile CorruptFile { what: \"Samsung v2: invalid difference length\", offset: None, reason: None }",
    "v2 opt=1 646x436 d=8 flip-opt ok () f1c88f74990aca05",
    "v2 opt=2 660x440 d=16 full ok () b6a53a5050f042d7",
    "v2 opt=2 660x440 d=16 short1 ok () b6a53a5050f042d7",
    "v2 opt=2 660x440 d=16 half CorruptFile CorruptFile { what: \"Samsung v2: truncated data at row 221/440\", offset: None, reason: None }",
    "v2 opt=2 660x440 d=16 tiny CorruptFile CorruptFile { what: \"Samsung v2: motion prediction on the first rows\", offset: None, reason: None }",
    "v2 opt=2 660x440 d=16 flip CorruptFile CorruptFile { what: \"Samsung v2: invalid difference length\", offset: None, reason: None }",
    "v2 opt=2 660x440 d=16 flip-opt ok () b6a53a5050f042d7",
    "v2 opt=4 646x436 d=12 full CorruptFile CorruptFile { what: \"Samsung v2: invalid difference length\", offset: None, reason: None }",
    "v2 opt=4 646x436 d=12 short1 CorruptFile CorruptFile { what: \"Samsung v2: invalid difference length\", offset: None, reason: None }",
    "v2 opt=4 646x436 d=12 half CorruptFile CorruptFile { what: \"Samsung v2: invalid difference length\", offset: None, reason: None }",
    "v2 opt=4 646x436 d=12 tiny CorruptFile CorruptFile { what: \"Samsung v2: truncated data at row 0/436\", offset: None, reason: None }",
    "v2 opt=4 646x436 d=12 flip CorruptFile CorruptFile { what: \"Samsung v2: invalid difference length\", offset: None, reason: None }",
    "v2 opt=4 646x436 d=12 flip-opt CorruptFile CorruptFile { what: \"Samsung v2: invalid difference length\", offset: None, reason: None }",
    "v2 opt=3 660x440 d=14 full ok () 9180af612da37c91",
    "v2 opt=3 660x440 d=14 short1 ok () 9180af612da37c91",
    "v2 opt=3 660x440 d=14 half CorruptFile CorruptFile { what: \"Samsung v2: truncated data at row 219/440\", offset: None, reason: None }",
    "v2 opt=3 660x440 d=14 tiny CorruptFile CorruptFile { what: \"Samsung v2: motion prediction on the first rows\", offset: None, reason: None }",
    "v2 opt=3 660x440 d=14 flip ok () 4d84aa0aec087377",
    "v2 opt=3 660x440 d=14 flip-opt ok () 9180af612da37c91",
    "v2 opt=6 646x436 d=8 full ok () 459a0d397a60568d",
    "v2 opt=6 646x436 d=8 short1 ok () 459a0d397a60568d",
    "v2 opt=6 646x436 d=8 half CorruptFile CorruptFile { what: \"Samsung v2: truncated data at row 220/436\", offset: None, reason: None }",
    "v2 opt=6 646x436 d=8 tiny CorruptFile CorruptFile { what: \"Samsung v2: motion prediction on the first rows\", offset: None, reason: None }",
    "v2 opt=6 646x436 d=8 flip ok () af210829157d4d5d",
    "v2 opt=6 646x436 d=8 flip-opt ok () 459a0d397a60568d",
    "v2 opt=7 660x440 d=16 full ok () 0e7c29070750d9ca",
    "v2 opt=7 660x440 d=16 short1 ok () 0e7c29070750d9ca",
    "v2 opt=7 660x440 d=16 half CorruptFile CorruptFile { what: \"Samsung v2: truncated data at row 218/440\", offset: None, reason: None }",
    "v2 opt=7 660x440 d=16 tiny CorruptFile CorruptFile { what: \"Samsung v2: motion prediction on the first rows\", offset: None, reason: None }",
    "v2 opt=7 660x440 d=16 flip ok () 91cdf40ae62b6ed3",
    "v2 opt=7 660x440 d=16 flip-opt ok () 0e7c29070750d9ca",
    "v2 opt=8 646x436 d=12 full CorruptFile CorruptFile { what: \"Samsung v2: invalid difference length\", offset: None, reason: None }",
    "v2 opt=8 646x436 d=12 short1 CorruptFile CorruptFile { what: \"Samsung v2: invalid difference length\", offset: None, reason: None }",
    "v2 opt=8 646x436 d=12 half CorruptFile CorruptFile { what: \"Samsung v2: invalid difference length\", offset: None, reason: None }",
    "v2 opt=8 646x436 d=12 tiny CorruptFile CorruptFile { what: \"Samsung v2: truncated data at row 0/436\", offset: None, reason: None }",
    "v2 opt=8 646x436 d=12 flip CorruptFile CorruptFile { what: \"Samsung v2: invalid difference length\", offset: None, reason: None }",
    "v2 opt=8 646x436 d=12 flip-opt CorruptFile CorruptFile { what: \"Samsung v2: invalid difference length\", offset: None, reason: None }",
    "v2 opt=13 660x440 d=14 full CorruptFile CorruptFile { what: \"Samsung v2: invalid difference length\", offset: None, reason: None }",
    "v2 opt=13 660x440 d=14 short1 CorruptFile CorruptFile { what: \"Samsung v2: invalid difference length\", offset: None, reason: None }",
    "v2 opt=13 660x440 d=14 half CorruptFile CorruptFile { what: \"Samsung v2: invalid difference length\", offset: None, reason: None }",
    "v2 opt=13 660x440 d=14 tiny CorruptFile CorruptFile { what: \"Samsung v2: truncated data at row 0/440\", offset: None, reason: None }",
    "v2 opt=13 660x440 d=14 flip CorruptFile CorruptFile { what: \"Samsung v2: invalid difference length\", offset: None, reason: None }",
    "v2 opt=13 660x440 d=14 flip-opt CorruptFile CorruptFile { what: \"Samsung v2: invalid difference length\", offset: None, reason: None }",
    "v2 geometry 645x436 CorruptFile CorruptFile { what: \"Samsung v2: motion prediction on the first rows\", offset: None, reason: None }",
    "v2 geometry 646x435 CorruptFile CorruptFile { what: \"Samsung v2: motion prediction on the first rows\", offset: None, reason: None }",
    "v2 geometry 15x436 CorruptFile CorruptFile { what: \"Samsung v2: image too small\", offset: None, reason: None }",
    "v2 geometry 646x0 CorruptFile CorruptFile { what: \"Samsung v2: image too small\", offset: None, reason: None }",
    "v2 geometry 0x0 CorruptFile CorruptFile { what: \"Samsung v2: image too small\", offset: None, reason: None }",
    "v2 geometry 646x436 InvalidArgument InvalidArgument(\"Samsung v2: output too small\")",
];
