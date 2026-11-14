//! Stream-level tests of the VC-5 decoder. The streams come from a generator below; the outcomes
//! (error kind, and a hash of the mosaic when decoding succeeds) of whole families of streams are
//! pinned by golden hashes. The goldens were recorded from a run in which the previous
//! implementation and this one agreed on every stream. Set `VC5_PRINT_GOLDEN=1` to print them.
//!
//! The streams come from a small VC-5 *generator* below (random coefficients, not a real wavelet
//! analysis: any coefficient set is a valid input to the inverse transform). Valid streams must give
//! identical output; mutated and truncated streams must give the same Ok/Err outcome and, when Ok,
//! identical output. (The contents of `out` after an Err are unspecified and not compared.)

use flashraw_codecs::vc5 as new;
use new::CfaPhase;

// ---------------------------------------------------------------- random numbers

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
    fn chance(&mut self, num: usize, den: usize) -> bool {
        self.below(den) < num
    }
}

// ---------------------------------------------------------------- ST 2073-1 Annex C codebook
// (length, codeword, run count, run value); the last entry is the band end marker (Table C.2).

#[rustfmt::skip]
const BOOK: [(u32, u32, u32, u32); 264] = [
    (1, 0x0, 1, 0), (7, 0x69, 12, 0), (8, 0xD1, 20, 0), (9, 0x18A, 32, 0),
    (10, 0x343, 60, 0), (11, 0x685, 100, 0), (13, 0x18BF, 180, 0), (13, 0x1BA5, 320, 0),
    (2, 0x2, 1, 1), (3, 0x7, 1, 2), (5, 0x19, 1, 3), (6, 0x30, 1, 4),
    (6, 0x36, 1, 5), (7, 0x63, 1, 6), (7, 0x6B, 1, 7), (7, 0x6F, 1, 8),
    (8, 0xD4, 1, 9), (8, 0xDC, 1, 10), (9, 0x189, 1, 11), (9, 0x1A0, 1, 12),
    (9, 0x1AB, 1, 13), (10, 0x310, 1, 14), (10, 0x316, 1, 15), (10, 0x354, 1, 16),
    (10, 0x375, 1, 17), (10, 0x377, 1, 18), (11, 0x623, 1, 19), (11, 0x684, 1, 20),
    (11, 0x6AB, 1, 21), (11, 0x6EC, 1, 22), (12, 0xC44, 1, 23), (12, 0xC5C, 1, 24),
    (12, 0xC5E, 1, 25), (12, 0xD55, 1, 26), (12, 0xDD1, 1, 27), (12, 0xDD3, 1, 28),
    (12, 0xDDB, 1, 29), (13, 0x188B, 1, 30), (13, 0x18BB, 1, 31), (13, 0x1AA8, 1, 32),
    (13, 0x1BA0, 1, 33), (13, 0x1BA4, 1, 34), (13, 0x1BB5, 1, 35), (14, 0x3115, 1, 36),
    (14, 0x3175, 1, 37), (14, 0x317D, 1, 38), (14, 0x3553, 1, 39), (14, 0x3768, 1, 40),
    (15, 0x6228, 1, 41), (15, 0x62E8, 1, 42), (15, 0x62F8, 1, 43), (15, 0x6AA4, 1, 44),
    (15, 0x6E85, 1, 45), (15, 0x6E87, 1, 46), (15, 0x6ED3, 1, 47), (16, 0xC453, 1, 48),
    (16, 0xC5D3, 1, 49), (16, 0xC5F3, 1, 50), (16, 0xDD08, 1, 51), (16, 0xDD0C, 1, 52),
    (16, 0xDDA4, 1, 53), (17, 0x188A4, 1, 54), (17, 0x18BA5, 1, 55), (17, 0x18BE5, 1, 56),
    (17, 0x1AA95, 1, 57), (17, 0x1AA97, 1, 58), (17, 0x1BA13, 1, 59), (17, 0x1BB4A, 1, 60),
    (17, 0x1BB4B, 1, 61), (18, 0x31748, 1, 62), (18, 0x317C8, 1, 63), (18, 0x35528, 1, 64),
    (18, 0x3552C, 1, 65), (18, 0x37424, 1, 66), (18, 0x37434, 1, 67), (18, 0x37436, 1, 68),
    (19, 0x62294, 1, 69), (19, 0x62E92, 1, 70), (19, 0x62F92, 1, 71), (19, 0x6AA52, 1, 72),
    (19, 0x6AA5A, 1, 73), (19, 0x6E84A, 1, 74), (19, 0x6E86A, 1, 75), (19, 0x6E86E, 1, 76),
    (20, 0xC452A, 1, 77), (20, 0xC5D27, 1, 78), (20, 0xC5F26, 1, 79), (20, 0xD54A6, 1, 80),
    (20, 0xD54B6, 1, 81), (20, 0xDD096, 1, 82), (20, 0xDD0D6, 1, 83), (20, 0xDD0DE, 1, 84),
    (21, 0x188A56, 1, 85), (21, 0x18BA4D, 1, 86), (21, 0x18BE4E, 1, 87), (21, 0x18BE4F, 1, 88),
    (21, 0x1AA96E, 1, 89), (21, 0x1BA12E, 1, 90), (21, 0x1BA12F, 1, 91), (21, 0x1BA1AF, 1, 92),
    (21, 0x1BA1BF, 1, 93), (22, 0x317498, 1, 94), (22, 0x35529C, 1, 95), (22, 0x35529D, 1, 96),
    (22, 0x3552DE, 1, 97), (22, 0x3552DF, 1, 98), (22, 0x37435D, 1, 99), (22, 0x37437D, 1, 100),
    (23, 0x62295D, 1, 101), (23, 0x62E933, 1, 102), (23, 0x6AA53D, 1, 103), (23, 0x6AA53E, 1, 104),
    (23, 0x6AA53F, 1, 105), (23, 0x6E86B9, 1, 106), (23, 0x6E86F8, 1, 107), (24, 0xC452B8, 1, 108),
    (24, 0xC5D265, 1, 109), (24, 0xD54A78, 1, 110), (24, 0xD54A79, 1, 111), (24, 0xDD0D70, 1, 112),
    (24, 0xDD0D71, 1, 113), (24, 0xDD0DF2, 1, 114), (24, 0xDD0DF3, 1, 115), (26, 0x3114BA2, 1, 116),
    (25, 0x188A5B1, 1, 117), (25, 0x188A58B, 1, 118), (25, 0x188A595, 1, 119), (25, 0x188A5D6, 1, 120),
    (25, 0x188A5D7, 1, 121), (25, 0x188A5A8, 1, 122), (25, 0x188A5AE, 1, 123), (25, 0x188A5AF, 1, 124),
    (25, 0x188A5C4, 1, 125), (25, 0x188A5C5, 1, 126), (25, 0x188A587, 1, 127), (25, 0x188A584, 1, 128),
    (25, 0x188A585, 1, 129), (25, 0x188A5C6, 1, 130), (25, 0x188A5C7, 1, 131), (25, 0x188A5CC, 1, 132),
    (25, 0x188A5CD, 1, 133), (25, 0x188A581, 1, 134), (25, 0x188A582, 1, 135), (25, 0x188A583, 1, 136),
    (25, 0x188A5CE, 1, 137), (25, 0x188A5CF, 1, 138), (25, 0x188A5C2, 1, 139), (25, 0x188A5C3, 1, 140),
    (25, 0x188A5C1, 1, 141), (25, 0x188A5B4, 1, 142), (25, 0x188A5B5, 1, 143), (25, 0x188A5E6, 1, 144),
    (25, 0x188A5E7, 1, 145), (25, 0x188A5E4, 1, 146), (25, 0x188A5E5, 1, 147), (25, 0x188A5AB, 1, 148),
    (25, 0x188A5E0, 1, 149), (25, 0x188A5E1, 1, 150), (25, 0x188A5E2, 1, 151), (25, 0x188A5E3, 1, 152),
    (25, 0x188A5B6, 1, 153), (25, 0x188A5B7, 1, 154), (25, 0x188A5FD, 1, 155), (25, 0x188A57E, 1, 156),
    (25, 0x188A57F, 1, 157), (25, 0x188A5EC, 1, 158), (25, 0x188A5ED, 1, 159), (25, 0x188A5FE, 1, 160),
    (25, 0x188A5FF, 1, 161), (25, 0x188A57D, 1, 162), (25, 0x188A59C, 1, 163), (25, 0x188A59D, 1, 164),
    (25, 0x188A5E8, 1, 165), (25, 0x188A5E9, 1, 166), (25, 0x188A5EA, 1, 167), (25, 0x188A5EB, 1, 168),
    (25, 0x188A5EF, 1, 169), (25, 0x188A57A, 1, 170), (25, 0x188A57B, 1, 171), (25, 0x188A578, 1, 172),
    (25, 0x188A579, 1, 173), (25, 0x188A5BA, 1, 174), (25, 0x188A5BB, 1, 175), (25, 0x188A5B8, 1, 176),
    (25, 0x188A5B9, 1, 177), (25, 0x188A588, 1, 178), (25, 0x188A589, 1, 179), (25, 0x18BA4C8, 1, 180),
    (25, 0x18BA4C9, 1, 181), (25, 0x188A5FA, 1, 182), (25, 0x188A5FB, 1, 183), (25, 0x188A5BC, 1, 184),
    (25, 0x188A5BD, 1, 185), (25, 0x188A598, 1, 186), (25, 0x188A599, 1, 187), (25, 0x188A5F4, 1, 188),
    (25, 0x188A5F5, 1, 189), (25, 0x188A59B, 1, 190), (25, 0x188A5DE, 1, 191), (25, 0x188A5DF, 1, 192),
    (25, 0x188A596, 1, 193), (25, 0x188A597, 1, 194), (25, 0x188A5F8, 1, 195), (25, 0x188A5F9, 1, 196),
    (25, 0x188A5F1, 1, 197), (25, 0x188A58E, 1, 198), (25, 0x188A58F, 1, 199), (25, 0x188A5DC, 1, 200),
    (25, 0x188A5DD, 1, 201), (25, 0x188A5F2, 1, 202), (25, 0x188A5F3, 1, 203), (25, 0x188A58C, 1, 204),
    (25, 0x188A58D, 1, 205), (25, 0x188A5A4, 1, 206), (25, 0x188A5F0, 1, 207), (25, 0x188A5A5, 1, 208),
    (25, 0x188A5A6, 1, 209), (25, 0x188A5A7, 1, 210), (25, 0x188A59A, 1, 211), (25, 0x188A5A2, 1, 212),
    (25, 0x188A5A3, 1, 213), (25, 0x188A58A, 1, 214), (25, 0x188A5B0, 1, 215), (25, 0x188A5A0, 1, 216),
    (25, 0x188A5A1, 1, 217), (25, 0x188A5DA, 1, 218), (25, 0x188A5DB, 1, 219), (25, 0x188A59E, 1, 220),
    (25, 0x188A59F, 1, 221), (25, 0x188A5D8, 1, 222), (25, 0x188A5EE, 1, 223), (25, 0x188A5D9, 1, 224),
    (25, 0x188A5F6, 1, 225), (25, 0x188A5F7, 1, 226), (25, 0x188A57C, 1, 227), (25, 0x188A5C8, 1, 228),
    (25, 0x188A5C9, 1, 229), (25, 0x188A594, 1, 230), (25, 0x188A5FC, 1, 231), (25, 0x188A5CA, 1, 232),
    (25, 0x188A5CB, 1, 233), (25, 0x188A5B2, 1, 234), (25, 0x188A5AA, 1, 235), (25, 0x188A5B3, 1, 236),
    (25, 0x188A572, 1, 237), (25, 0x188A573, 1, 238), (25, 0x188A5C0, 1, 239), (25, 0x188A5BE, 1, 240),
    (25, 0x188A5BF, 1, 241), (25, 0x188A592, 1, 242), (25, 0x188A580, 1, 243), (25, 0x188A593, 1, 244),
    (25, 0x188A590, 1, 245), (25, 0x188A591, 1, 246), (25, 0x188A586, 1, 247), (25, 0x188A5A9, 1, 248),
    (25, 0x188A5D2, 1, 249), (25, 0x188A5D3, 1, 250), (25, 0x188A5D4, 1, 251), (25, 0x188A5D5, 1, 252),
    (25, 0x188A5AC, 1, 253), (25, 0x188A5AD, 1, 254), (25, 0x188A5D0, 1, 255), (26, 0x3114BA3, 0, 256),
];

fn run_code(count: u32) -> (u32, u32) {
    let e = BOOK.iter().find(|e| e.2 == count && e.3 == 0).unwrap();
    (e.0, e.1)
}
fn mag_code(m: u32) -> (u32, u32) {
    let e = BOOK.iter().find(|e| e.2 == 1 && e.3 == m).unwrap();
    (e.0, e.1)
}

// ---------------------------------------------------------------- bit writer

#[derive(Default)]
struct BitW {
    bytes: Vec<u8>,
    acc: u64,
    n: u32,
}
impl BitW {
    fn put(&mut self, bits: u32, len: u32) {
        for i in (0..len).rev() {
            self.acc = (self.acc << 1) | ((bits >> i) & 1) as u64;
            self.n += 1;
            if self.n == 8 {
                self.bytes.push(self.acc as u8);
                self.acc = 0;
                self.n = 0;
            }
        }
    }
    /// Pad with zero bits to a multiple of `unit` bytes.
    fn finish(mut self, unit: usize) -> Vec<u8> {
        while self.n != 0 {
            self.put(0, 1);
        }
        while self.bytes.len() % unit != 0 {
            self.bytes.push(0);
        }
        self.bytes
    }
}

// ---------------------------------------------------------------- stream generator

#[derive(Clone)]
struct Params {
    width: usize,
    height: usize,
    /// Prescale tag values per channel.
    prescale: [u16; 4],
    precision: [u32; 4],
    /// Wrap codeblocks in GoPro-style optional large container chunks.
    containers: bool,
    /// Insert optional small chunks (skipped by size) and unknown optional tags.
    junk: bool,
    /// Realistic (smooth, in-range) lowpass values instead of random ones.
    smooth: bool,
    /// Write the subbands of the 4 channels in a shuffled order inside each channel.
    shuffle: bool,
    /// Emit the band end marker after each highpass band.
    marker: bool,
}

fn dims(w: usize, h: usize) -> [(usize, usize); 4] {
    let mut d = [(0, 0); 4];
    let (mut w, mut h) = (w, h);
    for e in d.iter_mut() {
        w = w.div_ceil(2);
        h = h.div_ceil(2);
        *e = (w, h);
    }
    d
}

fn tag(out: &mut Vec<u8>, t: i16, v: u16) {
    out.extend_from_slice(&t.to_be_bytes());
    out.extend_from_slice(&v.to_be_bytes());
}

fn encode_highpass(r: &mut Rng, n: usize, marker: bool) -> Vec<u8> {
    let mut w = BitW::default();
    // Sparse: long zero runs, small magnitudes, an occasional large one.
    let density = [2, 5, 20, 50][r.below(4)];
    let mut i = 0;
    while i < n {
        if r.chance(100 - density, 100) {
            let span = if r.chance(1, 3) { 700 } else { 40 };
            let mut z = (1 + r.below(span)).min(n - i);
            i += z;
            while z > 0 {
                let c = [320, 180, 100, 60, 32, 20, 12, 1].into_iter().find(|&c| c <= z).unwrap();
                let (l, b) = run_code(c as u32);
                w.put(b, l);
                z -= c;
            }
        } else {
            let m = if r.chance(1, 40) { 1 + r.below(255) } else { 1 + r.below(12) } as u32;
            let (l, b) = mag_code(m);
            w.put(b, l);
            w.put(r.below(2) as u32, 1);
            i += 1;
        }
    }
    if marker {
        let e = BOOK[263];
        w.put(e.1, e.0);
    }
    w.finish(4)
}

fn encode_lowpass(r: &mut Rng, n: usize, p: u32, smooth: bool) -> Vec<u8> {
    let mut w = BitW::default();
    let mut v: i64 = 1 << (p - 1).min(11);
    for _ in 0..n {
        let x = if smooth {
            v = (v + r.below(41) as i64 - 20).clamp(0, (1i64 << p) - 1);
            v as u32
        } else {
            (r.step() as u32) & ((1u32 << p) - 1)
        };
        w.put(x, p);
    }
    w.finish(8)
}

fn chunk(out: &mut Vec<u8>, payload: &[u8]) {
    let segs = payload.len() / 4;
    tag(out, (0x6000 | (segs >> 16)) as i16, (segs & 0xFFFF) as u16);
    out.extend_from_slice(payload);
}

fn gen_stream(r: &mut Rng, p: &Params) -> Vec<u8> {
    let mut s = b"VC-5".to_vec();
    let d = dims(p.width, p.height);
    tag(&mut s, 0x0c, 4);
    tag(&mut s, 0x14, p.width as u16);
    tag(&mut s, 0x15, p.height as u16);
    tag(&mut s, 0x54, 4);
    tag(&mut s, 0x6a, 2);
    tag(&mut s, 0x6b, 2);
    tag(&mut s, 0x6c, 1);
    tag(&mut s, 0x66, 12);
    if p.junk {
        tag(&mut s, -0x4004, 3);
        s.extend_from_slice(&[7u8; 12]);
        tag(&mut s, -0x55, 9);
    }
    for ch in 0..4usize {
        let mut order: Vec<usize> = (0..10).collect();
        if p.shuffle {
            for i in (1..10).rev() {
                order.swap(i, r.below(i + 1));
            }
        }
        tag(&mut s, 0x3e, ch as u16);
        tag(&mut s, 0x6d, p.prescale[ch]);
        for &sb in &order {
            let (level, band) = if sb == 0 { (2, 0) } else { (2 - (sb - 1) / 3, 1 + (sb - 1) % 3) };
            let (w, h) = d[level + 1];
            let _ = band;
            let mut body = Vec::new();
            tag(&mut body, 0x3e, ch as u16);
            tag(&mut body, 0x30, sb as u16);
            if sb == 0 {
                tag(&mut body, 0x23, p.precision[ch] as u16);
                chunk(&mut body, &encode_lowpass(r, w * h, p.precision[ch], p.smooth));
            } else {
                let q = [1u16, 12, 24, 3][r.below(4)];
                tag(&mut body, 0x35, q);
                chunk(&mut body, &encode_highpass(r, w * h, p.marker));
            }
            if p.containers {
                let segs = body.len() / 4;
                tag(&mut s, -((0x2000 | (segs >> 16)) as i16), (segs & 0xFFFF) as u16);
            }
            s.extend_from_slice(&body);
        }
    }
    s
}

fn default_params(r: &mut Rng) -> Params {
    let ps = [0x2800u16, 0x2800, 0, 0xA800, 0x2A00, 0x0A00, 0xFFFF, r.step() as u16];
    let mut prescale = [0x2800; 4];
    for x in prescale.iter_mut() {
        *x = ps[r.below(ps.len())];
    }
    let mut precision = [16u32; 4];
    for x in precision.iter_mut() {
        *x = [16, 16, 12, 14, 8, 9, 13][r.below(7)];
    }
    Params {
        width: 34 + 2 * r.below(60),
        height: 34 + 2 * r.below(60),
        prescale,
        precision,
        containers: r.chance(1, 2),
        junk: r.chance(1, 2),
        smooth: r.chance(1, 2),
        shuffle: r.chance(1, 3),
        marker: true,
    }
}

// ---------------------------------------------------------------- comparison

fn kind<T>(r: &flashraw_core::Result<T>) -> &'static str {
    match r {
        Ok(_) => "ok",
        Err(e) => e.kind(),
    }
}

thread_local! {
    /// Running hash of the outcomes of the current test.
    static ACC: std::cell::Cell<u64> = const { std::cell::Cell::new(0xcbf2_9ce4_8422_2325) };
}

fn fold(bytes: &[u8]) {
    ACC.with(|a| {
        let mut h = a.get();
        for &b in bytes {
            h = (h ^ b as u64).wrapping_mul(0x100_0000_01b3);
        }
        a.set(h);
    });
}

/// Check the recorded hash of this test's outcomes.
fn golden(name: &str, expected: u64) {
    let got = ACC.with(|a| a.get());
    if std::env::var("VC5_PRINT_GOLDEN").is_ok() {
        eprintln!("GOLDEN {name} {got:#018x}");
    } else {
        assert_eq!(got, expected, "{name}: outcomes changed");
    }
}

/// Decode and fold the outcome (error kind, or the mosaic) into the running hash.
fn compare(data: &[u8], w: usize, h: usize, white: u32, phase: CfaPhase, par: bool, what: &str) -> bool {
    let n = w * h;
    let mut a = vec![0xA5A5u16; n];
    let ra = new::decode(data, w, h, white, phase, &mut a, par);
    fold(kind(&ra).as_bytes());
    if ra.is_ok() {
        let bytes: Vec<u8> = a.iter().flat_map(|v| v.to_le_bytes()).collect();
        fold(&bytes);
    }
    // Parallel and sequential decoding agree.
    if ra.is_ok() && par {
        let mut b = vec![0xA5A5u16; n];
        new::decode(data, w, h, white, phase, &mut b, false).unwrap_or_else(|e| panic!("{what}: {e}"));
        assert!(a == b, "{what}: parallel and sequential output differ");
    }
    ra.is_ok()
}

fn phase_of(r: &mut Rng) -> CfaPhase {
    [CfaPhase::Rggb, CfaPhase::Grbg, CfaPhase::Gbrg, CfaPhase::Bggr][r.below(4)]
}
fn white_of(r: &mut Rng) -> u32 {
    [255, 1023, 4095, 4095, 16383, 16383, 65535, 1, 3, 1 + r.below(65535) as u32][r.below(10)]
}

fn mutate(r: &mut Rng, s: &mut Vec<u8>) {
    // Header-heavy: half of the mutations land in the first 200 bytes (tags), the rest anywhere.
    if s.len() < 8 {
        return;
    }
    let span = if r.chance(1, 2) { s.len().min(200) } else { s.len() };
    let pos = r.below(span);
    match r.below(7) {
        0 => s[pos] ^= 1 << r.below(8),
        1 => s[pos] = r.step() as u8,
        2 => s.truncate(pos),
        3 => {
            let at = pos & !3;
            let v = r.step() as u32;
            for k in 0..4 {
                if at + k < s.len() {
                    s[at + k] = (v >> (8 * k)) as u8;
                }
            }
        }
        4 => {
            let at = pos & !3;
            if at + 4 <= s.len() {
                s.drain(at..at + 4);
            }
        }
        5 => {
            let at = pos & !3;
            let ins: Vec<u8> = (0..4).map(|_| r.step() as u8).collect();
            s.splice(at..at, ins);
        }
        _ => {
            // Swap two tag values among the first 40 four-byte slots.
            let a = r.below(40.min(s.len() / 4)) * 4;
            let b = r.below(40.min(s.len() / 4)) * 4;
            for k in 0..4 {
                s.swap(a + k, b + k);
            }
        }
    }
}

// ---------------------------------------------------------------- tests

/// The generator itself: valid streams are accepted.
#[test]
fn generator_sanity() {
    let mut r = Rng(0x1234_5678_9ABC_DEF1);
    let mut ok = 0;
    for _ in 0..100 {
        let p = default_params(&mut r);
        let s = gen_stream(&mut r, &p);
        let mut out = vec![0u16; p.width * p.height];
        if new::decode(&s, p.width, p.height, 4095, CfaPhase::Rggb, &mut out, false).is_ok() {
            ok += 1;
        }
    }
    assert_eq!(ok, 100, "every generated stream must decode");
}

#[test]
fn valid_streams() {
    let mut r = Rng(0x1234_5678_9ABC_DEF1);
    let mut ok = 0;
    for it in 0..400 {
        let p = default_params(&mut r);
        let s = gen_stream(&mut r, &p);
        let (phase, white, par) = (phase_of(&mut r), white_of(&mut r), r.chance(1, 2));
        if compare(
            &s,
            p.width,
            p.height,
            white,
            phase,
            par,
            &format!("valid #{it} {}x{} white {white}", p.width, p.height),
        ) {
            ok += 1;
        }
    }
    assert!(ok > 300, "generator should mostly make decodable streams, got {ok}/400");
    golden("valid_streams", 0xe2ea4f57e5eb664b);
}

#[test]
fn valid_streams_larger() {
    let mut r = Rng(0xCAFE_F00D_1111_2222);
    for it in 0..12 {
        let mut p = default_params(&mut r);
        p.width = 200 + 2 * r.below(300);
        p.height = 200 + 2 * r.below(200);
        let s = gen_stream(&mut r, &p);
        let (phase, white) = (phase_of(&mut r), white_of(&mut r));
        assert!(compare(&s, p.width, p.height, white, phase, true, &format!("large #{it}")));
    }
    golden("valid_streams_larger", 0x51ee529897186160);
}

#[test]
fn odd_intermediate_sizes() {
    // Sizes whose half/quarter/eighth sizes are odd (cropped reconstruction), and the smallest legal sizes.
    let mut r = Rng(0x0DD5_17E5);
    for &(w, h) in
        &[(34usize, 34usize), (36, 34), (34, 38), (66, 66), (98, 70), (130, 34), (150, 50), (34, 150), (102, 102)]
    {
        for _ in 0..4 {
            let mut p = default_params(&mut r);
            p.width = w;
            p.height = h;
            let s = gen_stream(&mut r, &p);
            compare(&s, w, h, white_of(&mut r), phase_of(&mut r), r.chance(1, 2), &format!("{w}x{h}"));
        }
    }
    // Too small for the three-level transform: must fail identically.
    for &(w, h) in &[(2usize, 2usize), (16, 16), (32, 32), (32, 64), (64, 32), (33 + 1, 32)] {
        let mut p = default_params(&mut r);
        p.width = w;
        p.height = h;
        let s = gen_stream(&mut r, &p);
        compare(&s, w, h, 4095, CfaPhase::Rggb, false, &format!("small {w}x{h}"));
    }
    golden("odd_intermediate_sizes", 0x434e8e2164175c3c);
}

#[test]
fn mutated_streams() {
    let mut r = Rng(0xBAD5_EED5_0000_0001);
    let (mut ok, mut err) = (0, 0);
    for it in 0..4000 {
        let mut p = default_params(&mut r);
        p.width = 34 + 2 * r.below(20);
        p.height = 34 + 2 * r.below(20);
        let mut s = gen_stream(&mut r, &p);
        for _ in 0..1 + r.below(3) {
            mutate(&mut r, &mut s);
        }
        let (phase, white) = (phase_of(&mut r), white_of(&mut r));
        if compare(&s, p.width, p.height, white, phase, false, &format!("mutated #{it}")) {
            ok += 1;
        } else {
            err += 1;
        }
    }
    eprintln!("mutated streams: {ok} decoded, {err} rejected");
    golden("mutated_streams", 0x850a9d0a817c2449);
}

#[test]
fn truncated_streams() {
    let mut r = Rng(0x7777_0000_AAAA_5555);
    for _ in 0..3 {
        let mut p = default_params(&mut r);
        p.width = 34;
        p.height = 36;
        let s = gen_stream(&mut r, &p);
        let mut len = 0;
        while len <= s.len() {
            compare(
                &s[..len],
                p.width,
                p.height,
                4095,
                CfaPhase::Gbrg,
                false,
                &format!("truncated to {len}/{}", s.len()),
            );
            len += 1 + r.below(7);
        }
    }
    golden("truncated_streams", 0x6446600dae704a07);
}

#[test]
fn stream_structure_errors() {
    // Hand-made protocol violations on an otherwise valid stream.
    let mut r = Rng(0x5151_5151);
    let mut p = default_params(&mut r);
    p.width = 40;
    p.height = 40;
    p.junk = false;
    p.containers = false;
    p.shuffle = false;
    let base = gen_stream(&mut r, &p);
    let at = |s: &[u8], tagv: u16| {
        (0..s.len() / 4).map(|i| i * 4).find(|&i| s[i] == (tagv >> 8) as u8 && s[i + 1] == tagv as u8)
    };
    // Each (tag, replacement value) pair is applied to the first occurrence of the tag.
    let cases: &[(u16, u16)] = &[
        (0x000c, 3),
        (0x0014, 42),
        (0x0015, 42),
        (0x0054, 3),
        (0x0066, 10),
        (0x006a, 4),
        (0x006b, 4),
        (0x006c, 2),
        (0x0023, 7),
        (0x0023, 17),
        (0x003e, 4),
        (0x0030, 10),
        (0x0035, 0),
        (0x0035, 0xFFFF),
        (0x0035, 0x8000),
        (0x006d, 0x1234),
    ];
    for &(t, v) in cases {
        let mut s = base.clone();
        if let Some(i) = at(&s, t) {
            s[i + 2] = (v >> 8) as u8;
            s[i + 3] = v as u8;
        }
        compare(&s, 40, 40, 4095, CfaPhase::Rggb, false, &format!("tag {t:#x} = {v}"));
    }
    // Unknown required / optional tags, optional small chunk with a size beyond the end, duplicated band.
    for &(t, v) in &[
        (0x77i16, 0u16),
        (-0x77, 0),
        (0x4001, 5),
        (-0x4001, 0xFFFF),
        (0x2001, 0),
        (-0x2001, 7),
        (0x7000, 3),
        (0x6100, 1),
        (0x0e, 10),
        (-0x0e, 9),
    ] {
        let mut s = base.clone();
        let mut ins = Vec::new();
        tag(&mut ins, t, v);
        s.splice(4..4, ins);
        compare(&s, 40, 40, 4095, CfaPhase::Rggb, false, &format!("inserted tag {t:#x} {v}"));
    }
    // Wrong size / dimensions / white level arguments.
    for &(w, h, white) in &[
        (0usize, 40usize, 4095u32),
        (40, 0, 4095),
        (41, 40, 4095),
        (40, 41, 4095),
        (65536, 40, 4095),
        (40, 40, 0),
        (40, 40, 65536),
        (40, 40, 65535),
        (40, 40, 1),
    ] {
        compare(&base, w, h, white, CfaPhase::Rggb, false, &format!("args {w}x{h} white {white}"));
    }
    // Output buffer too small.
    let mut b = vec![0u16; 40 * 40 - 1];
    let rb = new::decode(&base, 40, 40, 4095, CfaPhase::Rggb, &mut b, false);
    assert_eq!(rb.unwrap_err().kind(), "InvalidArgument");
    // An output buffer larger than needed: samples beyond width*height stay untouched.
    let mut b = vec![0x5A5Au16; 40 * 40 + 7];
    new::decode(&base, 40, 40, 4095, CfaPhase::Bggr, &mut b, true).unwrap();
    assert!(b[1600..].iter().all(|&x| x == 0x5A5A));
    golden("stream_structure_errors", 0x3a25450fc2c44480);
}

// ---------------------------------------------------------------- real files

const CORPUS: &str = "/var/projects/flashraw-realworld/corpus";
/// (path, width, height, phase, white level) as reported by `flashraw info` for the pinned rows.
const FILES: &[(&str, usize, usize, CfaPhase, u32)] = &[
    ("GoPro/FUSION/GPBK1322.GPR", 3104, 3000, CfaPhase::Rggb, 16383),
    ("GoPro/HERO10 Black/GOPR0018.GPR", 5568, 4176, CfaPhase::Gbrg, 4095),
    ("GoPro/HERO11 Black/GOPR0123.GPR", 5568, 4872, CfaPhase::Gbrg, 4095),
    ("GoPro/HERO5 Black/GOPR0020.GPR", 4000, 3000, CfaPhase::Rggb, 16383),
    ("GoPro/HERO7 Black/GOPR0055.GPR", 4000, 3000, CfaPhase::Rggb, 16383),
    ("GoPro/HERO9 Black/GOPR0017.GPR", 5568, 4176, CfaPhase::Rggb, 16383),
];

fn stream_of(path: &str) -> Option<Vec<u8>> {
    let bytes = std::fs::read(format!("{CORPUS}/{path}")).ok()?;
    let i = bytes.windows(4).position(|w| w == b"VC-5")?;
    Some(bytes[i..].to_vec())
}

#[test]
#[ignore = "needs the GoPro corpus"]
fn real_files() {
    let mut seen = 0;
    for &(path, w, h, phase, white) in FILES {
        let Some(s) = stream_of(path) else { continue };
        seen += 1;
        assert!(compare(&s, w, h, white, phase, true, path), "{path} must decode");
        // The same stream with the other phases and bit depths.
        for ph in [CfaPhase::Grbg, CfaPhase::Bggr] {
            compare(&s, w, h, 16383, ph, true, path);
        }
        compare(&s, w, h, 255, CfaPhase::Rggb, true, path);
    }
    eprintln!("real files compared: {seen}/{}", FILES.len());
    golden("real_files", 0xfafa02ad7c4dc9b2);
}

#[test]
#[ignore = "needs the GoPro corpus"]
fn real_file_mutations() {
    let Some(s0) = stream_of("GoPro/HERO7 Black/GOPR0055.GPR") else { return };
    let mut r = Rng(0xFEED_FACE_0BAD_CAFE);
    for it in 0..40 {
        let mut s = s0.clone();
        for _ in 0..1 + r.below(2) {
            mutate(&mut r, &mut s);
        }
        compare(&s, 4000, 3000, 16383, CfaPhase::Rggb, true, &format!("real mutated #{it}"));
    }
    golden("real_file_mutations", 0xd8181b746fe0dd6e);
}
