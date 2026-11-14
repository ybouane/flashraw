//! VC-5 (SMPTE ST 2073-1) decoder for the GoPro `.GPR` flavour of DNG (compression 9).
//!
//! A GPR image of `W x H` samples is stored as four channels of `W/2 x H/2` values in a 12-bit
//! logarithmic domain. Each channel is a three-level 2/6 wavelet pyramid (one lowpass band and
//! three highpass bands per level, ten subbands). The decoder works in four steps:
//!
//! 1. [`Layout::parse`] walks the tag-value pairs and collects the 40 codeblocks;
//! 2. every band is decoded (raw lowpass values, or run-length/variable-length highpass
//!    coefficients that are inverse companded and dequantised);
//! 3. each channel runs the inverse wavelet from the coarsest level to the finest;
//! 4. the four channels are combined into the Bayer mosaic through the inverse log curve.
//!
//! The layout of the four channels in the mosaic is the GoPro/CineForm convention (the standard
//! leaves image repacking out of scope); the other conventions of the files (tag ranges that act
//! as containers, the prescale tag, the 8-byte padding of lowpass blocks) are noted where they
//! are used.

use crate::x3f::PrefixCode;
use flashraw_core::bits::{BitPumpMsb, BitRead};
use flashraw_core::{Error, Result, par};
use std::sync::OnceLock;

/// Bayer phase of the output mosaic: the colours of the top-left 2x2 block, row by row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CfaPhase {
    Rggb,
    Grbg,
    Gbrg,
    Bggr,
}

impl CfaPhase {
    /// The phase of a 2x2 Bayer pattern anchored at the image origin (`None` for other CFAs).
    pub fn from_pattern(p: &flashraw_core::CfaPattern) -> Option<Self> {
        use flashraw_core::CfaColor::{Blue, Green, Red};
        if !p.is_bayer() {
            return None;
        }
        // `color_at(x, y)`: the two pixels of the top row identify the phase.
        Some(match (p.color_at(0, 0), p.color_at(1, 0)) {
            (Red, Green) => CfaPhase::Rggb,
            (Green, Red) => CfaPhase::Grbg,
            (Green, Blue) => CfaPhase::Gbrg,
            (Blue, Green) => CfaPhase::Bggr,
            _ => return None,
        })
    }
}

/// Number of colour channels, wavelet levels and subbands of a GPR image.
const CHANNELS: usize = 4;
const LEVELS: usize = 3;
const SUBBANDS: usize = 10;

fn bad(what: &'static str) -> Error {
    Error::corrupt(what)
}

// ------------------------------------------------------------------------------------------
// Codebook (ST 2073-1 Annex C, Tables C.1 and C.2)
// ------------------------------------------------------------------------------------------

/// `(code length, codeword, run count, magnitude)`: magnitude 0 with run count n is a run of
/// n zeros, run count 1 with magnitude m is one coefficient followed by a sign bit.
const CODEBOOK: [(u8, u32, u16, u8); 263] = [
    (1, 0x0, 1, 0),
    (7, 0x69, 12, 0),
    (8, 0xd1, 20, 0),
    (9, 0x18a, 32, 0),
    (10, 0x343, 60, 0),
    (11, 0x685, 100, 0),
    (13, 0x18bf, 180, 0),
    (13, 0x1ba5, 320, 0),
    (2, 0x2, 1, 1),
    (3, 0x7, 1, 2),
    (5, 0x19, 1, 3),
    (6, 0x30, 1, 4),
    (6, 0x36, 1, 5),
    (7, 0x63, 1, 6),
    (7, 0x6b, 1, 7),
    (7, 0x6f, 1, 8),
    (8, 0xd4, 1, 9),
    (8, 0xdc, 1, 10),
    (9, 0x189, 1, 11),
    (9, 0x1a0, 1, 12),
    (9, 0x1ab, 1, 13),
    (10, 0x310, 1, 14),
    (10, 0x316, 1, 15),
    (10, 0x354, 1, 16),
    (10, 0x375, 1, 17),
    (10, 0x377, 1, 18),
    (11, 0x623, 1, 19),
    (11, 0x684, 1, 20),
    (11, 0x6ab, 1, 21),
    (11, 0x6ec, 1, 22),
    (12, 0xc44, 1, 23),
    (12, 0xc5c, 1, 24),
    (12, 0xc5e, 1, 25),
    (12, 0xd55, 1, 26),
    (12, 0xdd1, 1, 27),
    (12, 0xdd3, 1, 28),
    (12, 0xddb, 1, 29),
    (13, 0x188b, 1, 30),
    (13, 0x18bb, 1, 31),
    (13, 0x1aa8, 1, 32),
    (13, 0x1ba0, 1, 33),
    (13, 0x1ba4, 1, 34),
    (13, 0x1bb5, 1, 35),
    (14, 0x3115, 1, 36),
    (14, 0x3175, 1, 37),
    (14, 0x317d, 1, 38),
    (14, 0x3553, 1, 39),
    (14, 0x3768, 1, 40),
    (15, 0x6228, 1, 41),
    (15, 0x62e8, 1, 42),
    (15, 0x62f8, 1, 43),
    (15, 0x6aa4, 1, 44),
    (15, 0x6e85, 1, 45),
    (15, 0x6e87, 1, 46),
    (15, 0x6ed3, 1, 47),
    (16, 0xc453, 1, 48),
    (16, 0xc5d3, 1, 49),
    (16, 0xc5f3, 1, 50),
    (16, 0xdd08, 1, 51),
    (16, 0xdd0c, 1, 52),
    (16, 0xdda4, 1, 53),
    (17, 0x188a4, 1, 54),
    (17, 0x18ba5, 1, 55),
    (17, 0x18be5, 1, 56),
    (17, 0x1aa95, 1, 57),
    (17, 0x1aa97, 1, 58),
    (17, 0x1ba13, 1, 59),
    (17, 0x1bb4a, 1, 60),
    (17, 0x1bb4b, 1, 61),
    (18, 0x31748, 1, 62),
    (18, 0x317c8, 1, 63),
    (18, 0x35528, 1, 64),
    (18, 0x3552c, 1, 65),
    (18, 0x37424, 1, 66),
    (18, 0x37434, 1, 67),
    (18, 0x37436, 1, 68),
    (19, 0x62294, 1, 69),
    (19, 0x62e92, 1, 70),
    (19, 0x62f92, 1, 71),
    (19, 0x6aa52, 1, 72),
    (19, 0x6aa5a, 1, 73),
    (19, 0x6e84a, 1, 74),
    (19, 0x6e86a, 1, 75),
    (19, 0x6e86e, 1, 76),
    (20, 0xc452a, 1, 77),
    (20, 0xc5d27, 1, 78),
    (20, 0xc5f26, 1, 79),
    (20, 0xd54a6, 1, 80),
    (20, 0xd54b6, 1, 81),
    (20, 0xdd096, 1, 82),
    (20, 0xdd0d6, 1, 83),
    (20, 0xdd0de, 1, 84),
    (21, 0x188a56, 1, 85),
    (21, 0x18ba4d, 1, 86),
    (21, 0x18be4e, 1, 87),
    (21, 0x18be4f, 1, 88),
    (21, 0x1aa96e, 1, 89),
    (21, 0x1ba12e, 1, 90),
    (21, 0x1ba12f, 1, 91),
    (21, 0x1ba1af, 1, 92),
    (21, 0x1ba1bf, 1, 93),
    (22, 0x317498, 1, 94),
    (22, 0x35529c, 1, 95),
    (22, 0x35529d, 1, 96),
    (22, 0x3552de, 1, 97),
    (22, 0x3552df, 1, 98),
    (22, 0x37435d, 1, 99),
    (22, 0x37437d, 1, 100),
    (23, 0x62295d, 1, 101),
    (23, 0x62e933, 1, 102),
    (23, 0x6aa53d, 1, 103),
    (23, 0x6aa53e, 1, 104),
    (23, 0x6aa53f, 1, 105),
    (23, 0x6e86b9, 1, 106),
    (23, 0x6e86f8, 1, 107),
    (24, 0xc452b8, 1, 108),
    (24, 0xc5d265, 1, 109),
    (24, 0xd54a78, 1, 110),
    (24, 0xd54a79, 1, 111),
    (24, 0xdd0d70, 1, 112),
    (24, 0xdd0d71, 1, 113),
    (24, 0xdd0df2, 1, 114),
    (24, 0xdd0df3, 1, 115),
    (26, 0x3114ba2, 1, 116),
    (25, 0x188a5b1, 1, 117),
    (25, 0x188a58b, 1, 118),
    (25, 0x188a595, 1, 119),
    (25, 0x188a5d6, 1, 120),
    (25, 0x188a5d7, 1, 121),
    (25, 0x188a5a8, 1, 122),
    (25, 0x188a5ae, 1, 123),
    (25, 0x188a5af, 1, 124),
    (25, 0x188a5c4, 1, 125),
    (25, 0x188a5c5, 1, 126),
    (25, 0x188a587, 1, 127),
    (25, 0x188a584, 1, 128),
    (25, 0x188a585, 1, 129),
    (25, 0x188a5c6, 1, 130),
    (25, 0x188a5c7, 1, 131),
    (25, 0x188a5cc, 1, 132),
    (25, 0x188a5cd, 1, 133),
    (25, 0x188a581, 1, 134),
    (25, 0x188a582, 1, 135),
    (25, 0x188a583, 1, 136),
    (25, 0x188a5ce, 1, 137),
    (25, 0x188a5cf, 1, 138),
    (25, 0x188a5c2, 1, 139),
    (25, 0x188a5c3, 1, 140),
    (25, 0x188a5c1, 1, 141),
    (25, 0x188a5b4, 1, 142),
    (25, 0x188a5b5, 1, 143),
    (25, 0x188a5e6, 1, 144),
    (25, 0x188a5e7, 1, 145),
    (25, 0x188a5e4, 1, 146),
    (25, 0x188a5e5, 1, 147),
    (25, 0x188a5ab, 1, 148),
    (25, 0x188a5e0, 1, 149),
    (25, 0x188a5e1, 1, 150),
    (25, 0x188a5e2, 1, 151),
    (25, 0x188a5e3, 1, 152),
    (25, 0x188a5b6, 1, 153),
    (25, 0x188a5b7, 1, 154),
    (25, 0x188a5fd, 1, 155),
    (25, 0x188a57e, 1, 156),
    (25, 0x188a57f, 1, 157),
    (25, 0x188a5ec, 1, 158),
    (25, 0x188a5ed, 1, 159),
    (25, 0x188a5fe, 1, 160),
    (25, 0x188a5ff, 1, 161),
    (25, 0x188a57d, 1, 162),
    (25, 0x188a59c, 1, 163),
    (25, 0x188a59d, 1, 164),
    (25, 0x188a5e8, 1, 165),
    (25, 0x188a5e9, 1, 166),
    (25, 0x188a5ea, 1, 167),
    (25, 0x188a5eb, 1, 168),
    (25, 0x188a5ef, 1, 169),
    (25, 0x188a57a, 1, 170),
    (25, 0x188a57b, 1, 171),
    (25, 0x188a578, 1, 172),
    (25, 0x188a579, 1, 173),
    (25, 0x188a5ba, 1, 174),
    (25, 0x188a5bb, 1, 175),
    (25, 0x188a5b8, 1, 176),
    (25, 0x188a5b9, 1, 177),
    (25, 0x188a588, 1, 178),
    (25, 0x188a589, 1, 179),
    (25, 0x18ba4c8, 1, 180),
    (25, 0x18ba4c9, 1, 181),
    (25, 0x188a5fa, 1, 182),
    (25, 0x188a5fb, 1, 183),
    (25, 0x188a5bc, 1, 184),
    (25, 0x188a5bd, 1, 185),
    (25, 0x188a598, 1, 186),
    (25, 0x188a599, 1, 187),
    (25, 0x188a5f4, 1, 188),
    (25, 0x188a5f5, 1, 189),
    (25, 0x188a59b, 1, 190),
    (25, 0x188a5de, 1, 191),
    (25, 0x188a5df, 1, 192),
    (25, 0x188a596, 1, 193),
    (25, 0x188a597, 1, 194),
    (25, 0x188a5f8, 1, 195),
    (25, 0x188a5f9, 1, 196),
    (25, 0x188a5f1, 1, 197),
    (25, 0x188a58e, 1, 198),
    (25, 0x188a58f, 1, 199),
    (25, 0x188a5dc, 1, 200),
    (25, 0x188a5dd, 1, 201),
    (25, 0x188a5f2, 1, 202),
    (25, 0x188a5f3, 1, 203),
    (25, 0x188a58c, 1, 204),
    (25, 0x188a58d, 1, 205),
    (25, 0x188a5a4, 1, 206),
    (25, 0x188a5f0, 1, 207),
    (25, 0x188a5a5, 1, 208),
    (25, 0x188a5a6, 1, 209),
    (25, 0x188a5a7, 1, 210),
    (25, 0x188a59a, 1, 211),
    (25, 0x188a5a2, 1, 212),
    (25, 0x188a5a3, 1, 213),
    (25, 0x188a58a, 1, 214),
    (25, 0x188a5b0, 1, 215),
    (25, 0x188a5a0, 1, 216),
    (25, 0x188a5a1, 1, 217),
    (25, 0x188a5da, 1, 218),
    (25, 0x188a5db, 1, 219),
    (25, 0x188a59e, 1, 220),
    (25, 0x188a59f, 1, 221),
    (25, 0x188a5d8, 1, 222),
    (25, 0x188a5ee, 1, 223),
    (25, 0x188a5d9, 1, 224),
    (25, 0x188a5f6, 1, 225),
    (25, 0x188a5f7, 1, 226),
    (25, 0x188a57c, 1, 227),
    (25, 0x188a5c8, 1, 228),
    (25, 0x188a5c9, 1, 229),
    (25, 0x188a594, 1, 230),
    (25, 0x188a5fc, 1, 231),
    (25, 0x188a5ca, 1, 232),
    (25, 0x188a5cb, 1, 233),
    (25, 0x188a5b2, 1, 234),
    (25, 0x188a5aa, 1, 235),
    (25, 0x188a5b3, 1, 236),
    (25, 0x188a572, 1, 237),
    (25, 0x188a573, 1, 238),
    (25, 0x188a5c0, 1, 239),
    (25, 0x188a5be, 1, 240),
    (25, 0x188a5bf, 1, 241),
    (25, 0x188a592, 1, 242),
    (25, 0x188a580, 1, 243),
    (25, 0x188a593, 1, 244),
    (25, 0x188a590, 1, 245),
    (25, 0x188a591, 1, 246),
    (25, 0x188a586, 1, 247),
    (25, 0x188a5a9, 1, 248),
    (25, 0x188a5d2, 1, 249),
    (25, 0x188a5d3, 1, 250),
    (25, 0x188a5d4, 1, 251),
    (25, 0x188a5d5, 1, 252),
    (25, 0x188a5ac, 1, 253),
    (25, 0x188a5ad, 1, 254),
    (25, 0x188a5d0, 1, 255),
];

/// Band end marker (Table C.2): length and codeword.
const END_MARKER: (u32, u32) = (26, 0x3114BA3);

/// Width of the direct lookup of [`Codec::fast`].
const FAST_BITS: u32 = 12;

/// Entry of the direct lookup, indexed by the next `FAST_BITS` bits: bits 0..16 the signed,
/// inverse companded value, bits 16..25 the number of coefficients the entry advances (a zero
/// run, or 1 for a coefficient), bits 27..32 the bits consumed (sign included); 0 consumed
/// means "not resolved here".
type FastEntry = u32;

/// Decoding tables shared by every call.
struct Codec {
    fast: Vec<FastEntry>,
    /// Slow path for the codewords longer than the direct lookup. The symbol is
    /// `magnitude << 9 | run count`; the band end marker is symbol 0.
    long: PrefixCode,
    /// `T[i]` of the inverse log curve at 16 bits.
    curve: Vec<u16>,
}

/// Inverse companding (Annex F): `m + floor(768 m^3 / 255^3)`.
fn expand(m: u32) -> u32 {
    m + (u64::from(m).pow(3) * 768 / 16_581_375) as u32
}

fn codec() -> &'static Codec {
    static CODEC: OnceLock<Codec> = OnceLock::new();
    CODEC.get_or_init(|| {
        let mut symbols = Vec::with_capacity(CODEBOOK.len() + 1);
        let mut fast = vec![0u32; 1 << FAST_BITS];
        for &(len, code, run, mag) in CODEBOOK.iter() {
            let (len, mag, run) = (len as u32, mag as u32, run as u32);
            let m = expand(mag);
            symbols.push((code, len, m << 9 | run));
            let mut put = |bits: u32, used: u32, value: i32, advance: u32| {
                let free = FAST_BITS - used;
                let first = (bits << free) as usize;
                let e = (used << 27) | (advance << 16) | (value as i16 as u16 as u32);
                fast[first..first + (1usize << free)].fill(e);
            };
            if mag == 0 {
                if len <= FAST_BITS {
                    put(code, len, 0, run);
                }
            } else if len < FAST_BITS {
                put(code << 1, len + 1, m as i32, 1);
                put(code << 1 | 1, len + 1, -(m as i32), 1);
            }
        }
        symbols.push((END_MARKER.1, END_MARKER.0, 0));
        let long = PrefixCode::new(&symbols).expect("the VC-5 codebook is a prefix code");
        let curve = (0..4096)
            .map(|i| {
                let x = 113f64.powf(i as f64 / 4095.0);
                (65535.0 * (x - 1.0) / 112.0).floor() as u16
            })
            .collect();
        Codec { fast, long, curve }
    })
}

// ------------------------------------------------------------------------------------------
// Stream layout: tags and codeblocks
// ------------------------------------------------------------------------------------------

/// Sizes `(width, height)` of the bands of the three levels, finest first.
fn level_sizes(cw: usize, ch: usize) -> [(usize, usize); LEVELS] {
    let d0 = (cw.div_ceil(2), ch.div_ceil(2));
    let d1 = (d0.0.div_ceil(2), d0.1.div_ceil(2));
    let d2 = (d1.0.div_ceil(2), d1.1.div_ceil(2));
    [d0, d1, d2]
}

/// Level (0 = finest) of subband `s`; subband 0 is the lowpass band of the coarsest level.
fn subband_level(s: usize) -> usize {
    if s == 0 { LEVELS - 1 } else { LEVELS - 1 - (s - 1) / 3 }
}

/// One codeblock: its payload and the parameter in force (lowpass precision or quantisation).
#[derive(Clone, Copy)]
struct Block<'a> {
    payload: &'a [u8],
    param: i32,
}

/// The codeblocks of a stream by `[channel][subband]`, and the prescale words of the channels.
struct Layout<'a> {
    blocks: [[Option<Block<'a>>; SUBBANDS]; CHANNELS],
    prescale: [u16; CHANNELS],
}

impl<'a> Layout<'a> {
    /// Walk the tag-value pairs of `data` until all four channels are complete.
    fn parse(data: &'a [u8], width: usize, height: usize, sizes: &[(usize, usize); LEVELS]) -> Result<Self> {
        if data.get(..4) != Some(b"VC-5") {
            return Err(bad("VC-5: missing start marker"));
        }
        let mut lay = Layout { blocks: [[None; SUBBANDS]; CHANNELS], prescale: [0; CHANNELS] };
        let (mut channel, mut pending_sub) = (0usize, None::<usize>);
        let (mut precision, mut quant) = (None::<u32>, None::<i32>);
        let mut missing = CHANNELS * SUBBANDS;
        let mut at = 4usize;
        while missing > 0 {
            let seg = data.get(at..).and_then(|d| d.get(..4)).ok_or_else(|| bad("VC-5: truncated stream"))?;
            let tag = i16::from_be_bytes([seg[0], seg[1]]);
            let v = u16::from_be_bytes([seg[2], seg[3]]);
            at += 4;
            let optional = tag < 0;
            let t = tag.unsigned_abs() as u32;
            let want = |ok: bool| if ok { Ok(()) } else { Err(bad("VC-5: unsupported parameter")) };
            match t {
                0x0C => want(v == 4)?,
                0x14 => want(v as usize == width)?,
                0x15 => want(v as usize == height)?,
                0x0E => want(v == 10)?,
                0x54 => want(v == 4)?,
                0x66 => want(v == 12)?,
                0x6A | 0x6B => want(v == 2)?,
                0x6C => want(v == 1)?,
                0x23 => {
                    want((8..=16).contains(&v))?;
                    precision = Some(v as u32);
                }
                0x3E => {
                    want(v < 4)?;
                    channel = v as usize;
                }
                0x30 => {
                    want(v < 10)?;
                    pending_sub = Some(v as usize);
                }
                0x35 => quant = Some(v as i16 as i32),
                0x6D => lay.prescale[channel] = v,
                0x6000..=0x7FFF => {
                    let bytes = ((((t & 0xFF) as usize) << 16) | v as usize) * 4;
                    let payload = data
                        .get(at..)
                        .and_then(|d| d.get(..bytes))
                        .ok_or_else(|| bad("VC-5: codeblock exceeds the data"))?;
                    at += bytes;
                    let s = pending_sub.take().ok_or_else(|| bad("VC-5: codeblock without subband"))?;
                    if lay.blocks[channel][s].is_some() {
                        return Err(bad("VC-5: duplicate codeblock"));
                    }
                    let block = if s == 0 {
                        let p = precision.take().ok_or_else(|| bad("VC-5: lowpass without precision"))?;
                        let (w, h) = sizes[LEVELS - 1];
                        let used = (w * h * p as usize).div_ceil(64) * 8;
                        let payload = payload.get(..used).ok_or_else(|| bad("VC-5: lowpass block too short"))?;
                        Block { payload, param: p as i32 }
                    } else {
                        let q = quant.take().ok_or_else(|| bad("VC-5: highpass without quantisation"))?;
                        Block { payload, param: q }
                    };
                    lay.blocks[channel][s] = Some(block);
                    missing -= 1;
                }
                // Containers: the size is ignored and their contents are read as tags.
                0x2000..=0x3FFF => {}
                0x4000..=0x5FFF => {
                    if !optional {
                        return Err(bad("VC-5: required chunk tag not supported"));
                    }
                    at = at.saturating_add(v as usize * 4);
                }
                _ => {
                    if !optional {
                        return Err(bad("VC-5: required tag not supported"));
                    }
                }
            }
        }
        Ok(lay)
    }
}

// ------------------------------------------------------------------------------------------
// Band decoding
// ------------------------------------------------------------------------------------------

/// Lowpass band: `n` raw values of `precision` bits.
fn decode_lowpass(b: Block, n: usize) -> Vec<i16> {
    let mut bits = BitPumpMsb::new(b.payload);
    let p = b.param as u32;
    (0..n).map(|_| bits.get(p) as u16 as i16).collect()
}

#[cold]
fn overflow() -> Error {
    bad("VC-5: dequantisation overflow")
}

/// Highpass band of `n` coefficients, dequantised by `q`.
fn decode_highpass(b: Block, n: usize, codec: &Codec) -> Result<Vec<i16>> {
    let total_bits = b.payload.len() as u64 * 8;
    // Each codeword covers at most 320 coefficients, and the end marker takes 26 bits.
    if total_bits < n.div_ceil(320) as u64 + END_MARKER.0 as u64 {
        return Err(bad("VC-5: highpass block too short"));
    }
    let q = b.param;
    let mut out = vec![0i16; n];
    let mut bits = BitPumpMsb::new(b.payload);
    let mut pos = 0usize;
    while pos < n {
        bits.fill();
        let e = codec.fast[bits.peek_no_fill(FAST_BITS) as usize];
        let used = e >> 27;
        let (value, advance) = if used != 0 {
            bits.skip_no_fill(used);
            (e as u16 as i16 as i32, (e >> 16) & 0x1FF)
        } else {
            let sym = codec.long.decode(&mut bits)?;
            let (mag, run) = ((sym >> 9) as i32, sym & 0x1FF);
            if run == 0 {
                return Err(bad("VC-5: band ends early"));
            }
            if mag == 0 {
                (0, run)
            } else if bits.get(1) == 0 {
                (mag, 1)
            } else {
                (-mag, 1)
            }
        };
        let c = value * q;
        if c as i16 as i32 != c {
            return Err(overflow());
        }
        out[pos] = c as i16;
        pos += advance as usize;
        if pos > n {
            return Err(bad("VC-5: run exceeds the band"));
        }
    }
    if codec.long.decode(&mut bits)? != 0 {
        return Err(bad("VC-5: missing band end marker"));
    }
    if bits.bit_position() > total_bits {
        return Err(bad("VC-5: band ends beyond its codeblock"));
    }
    if bits.get(1) != 0 {
        return Err(bad("VC-5: nonzero padding after the band end marker"));
    }
    Ok(out)
}

// ------------------------------------------------------------------------------------------
// Inverse wavelet
// ------------------------------------------------------------------------------------------

/// One output sample of the 2/6 synthesis for a whole row slice (Annex A):
/// `((((k0 a + k1 b + k2 c + 4) >> 3) + hs h) << S) >> 1`, truncated to 16 bits, or clamped to
/// the 14-bit range when `CLAMP` is set. `S` is the prescale shift of the horizontal pass.
#[inline(always)]
#[allow(clippy::too_many_arguments)]
fn synth<const S: u32, const CLAMP: bool>(
    out: &mut [i16],
    a: &[i16],
    b: &[i16],
    c: &[i16],
    h: &[i16],
    k: [i32; 3],
    hs: i32,
) {
    let m = out.len();
    let (a, b, c, h) = (&a[..m], &b[..m], &c[..m], &h[..m]);
    for j in 0..m {
        let low = (k[0] * a[j] as i32 + k[1] * b[j] as i32 + k[2] * c[j] as i32 + 4) >> 3;
        let y = ((low + hs * h[j] as i32) << S) >> 1;
        out[j] = if CLAMP { y.clamp(0, 16383) as i16 } else { y as i16 };
    }
}

/// Coefficients of the three kinds of output position: `[even, odd]` each `(taps, sign of H)`.
type Taps = ([i32; 3], i32);
const FIRST: [Taps; 2] = [([11, -4, 1], 1), ([5, 4, -1], -1)];
const INTERIOR: [Taps; 2] = [([1, 8, -1], 1), ([-1, 8, 1], -1)];
const LAST: [Taps; 2] = [([-1, 4, 5], 1), ([1, -4, 11], -1)];

/// Vertical inverse for output row `r` of the `2n`-row result: `lo` holds the lowpass rows
/// (`stride` apart), `hi` is the highpass row `r / 2`. Writes `dst.len()` values into `dst`.
#[inline(always)]
fn vertical_row(dst: &mut [i16], r: usize, n: usize, lo: &[i16], stride: usize, hi: &[i16]) {
    let i = r >> 1;
    let parity = r & 1;
    let (rows, taps) = if i == 0 {
        ([0, 1, 2], FIRST[parity])
    } else if i == n - 1 {
        ([n - 3, n - 2, n - 1], LAST[parity])
    } else {
        ([i - 1, i, i + 1], INTERIOR[parity])
    };
    let row = |i: usize| &lo[i * stride..];
    synth::<0, false>(dst, row(rows[0]), row(rows[1]), row(rows[2]), hi, taps.0, taps.1);
}

/// Horizontal inverse of one row: lowpass `l`, highpass `h` (both `n` long) to `2n` samples.
#[inline(always)]
fn horizontal_row<const S: u32, const CLAMP: bool>(dst: &mut [i16], l: &[i16], h: &[i16], tmp: &mut [i16]) {
    let n = l.len();
    let (ev, od) = tmp.split_at_mut(n);
    let od = &mut od[..n];
    let edge = |ev: &mut [i16], od: &mut [i16], a: usize, e: usize, t: [Taps; 2]| {
        let sl = |o: usize| &l[a + o..a + o + 1];
        synth::<S, CLAMP>(&mut ev[e..e + 1], sl(0), sl(1), sl(2), &h[e..e + 1], t[0].0, t[0].1);
        synth::<S, CLAMP>(&mut od[e..e + 1], sl(0), sl(1), sl(2), &h[e..e + 1], t[1].0, t[1].1);
    };
    edge(ev, od, 0, 0, FIRST);
    edge(ev, od, n - 3, n - 1, LAST);
    let (a, b, c) = (&l[..n - 2], &l[1..n - 1], &l[2..n]);
    synth::<S, CLAMP>(&mut ev[1..n - 1], a, b, c, &h[1..n - 1], INTERIOR[0].0, INTERIOR[0].1);
    synth::<S, CLAMP>(&mut od[1..n - 1], a, b, c, &h[1..n - 1], INTERIOR[1].0, INTERIOR[1].1);
    for ((d, &e), &o) in dst.chunks_exact_mut(2).zip(ev.iter()).zip(od.iter()) {
        d[0] = e;
        d[1] = o;
    }
}

/// A plane of 16-bit samples.
struct Plane {
    data: Vec<i16>,
    stride: usize,
}

/// The bands of one wavelet level (all `w x h`).
struct LevelBands<'a> {
    w: usize,
    h: usize,
    lh: &'a [i16],
    hl: &'a [i16],
    hh: &'a [i16],
}

/// One level of the inverse transform: `ll` (at least `w x h`) and the three highpass bands to
/// a plane of `2w` columns and `rows` rows (`rows <= 2h`). `shift` is the prescale shift of the
/// horizontal pass; `last` marks the finest level, whose output is clamped to 14 bits.
fn inverse_level(ll: &Plane, bands: &LevelBands, shift: u32, last: bool, rows: usize, parallel: bool) -> Plane {
    let (w, h) = (bands.w, bands.h);
    let stride = 2 * w;
    let mut out = vec![0i16; stride * rows];
    let per_chunk = (rows / (par::threads() * 4).max(1)).clamp(16, 256) & !1;
    par::for_each_row_chunk(&mut out, stride, per_chunk, parallel, |first, chunk| {
        let mut lrow = vec![0i16; w];
        let mut hrow = vec![0i16; w];
        let mut tmp = vec![0i16; 2 * w];
        for (k, dst) in chunk.chunks_exact_mut(stride).enumerate() {
            let r = first + k;
            let i = r >> 1;
            vertical_row(&mut lrow, r, h, &ll.data, ll.stride, &bands.hl[i * w..(i + 1) * w]);
            vertical_row(&mut hrow, r, h, bands.lh, w, &bands.hh[i * w..(i + 1) * w]);
            match (shift, last) {
                (2, true) => horizontal_row::<2, true>(dst, &lrow, &hrow, &mut tmp),
                (2, false) => horizontal_row::<2, false>(dst, &lrow, &hrow, &mut tmp),
                (_, true) => horizontal_row::<0, true>(dst, &lrow, &hrow, &mut tmp),
                (_, false) => horizontal_row::<0, false>(dst, &lrow, &hrow, &mut tmp),
            }
        }
    });
    Plane { data: out, stride }
}

/// Prescale shift of `level` (0 = finest) from the prescale word: a 2-bit field per level in
/// bits 15..14, 13..12 and 11..10; the value 2 means a left shift by two, anything else none.
fn prescale_shift(word: u16, level: usize) -> u32 {
    if (word >> (14 - 2 * level)) & 3 == 2 { 2 } else { 0 }
}

/// Run the whole pyramid of one channel. Returns the plane (at least `cw x ch`).
fn inverse_channel(
    mut bands: Vec<Vec<i16>>,
    sizes: &[(usize, usize); LEVELS],
    ch: usize,
    prescale: u16,
    parallel: bool,
) -> Plane {
    let w2 = sizes[LEVELS - 1].0;
    let mut low = Plane { data: std::mem::take(&mut bands[0]), stride: w2 };
    for level in (0..LEVELS).rev() {
        let (w, h) = sizes[level];
        let first = 1 + 3 * (LEVELS - 1 - level);
        let lb = LevelBands { w, h, lh: &bands[first], hl: &bands[first + 1], hh: &bands[first + 2] };
        // Only the rows the next level (or the image) reads are needed.
        let rows = if level == 0 { ch } else { sizes[level - 1].1 };
        low = inverse_level(&low, &lb, prescale_shift(prescale, level), level == 0, rows.min(2 * h), parallel);
    }
    low
}

// ------------------------------------------------------------------------------------------
// Mosaic reconstruction
// ------------------------------------------------------------------------------------------

/// Combine the four channel planes into the mosaic rows `first_pair * 2 ..` held by `chunk`.
/// `PH` is the phase index (0 Rggb, 1 Grbg, 2 Gbrg, 3 Bggr).
fn mosaic_rows<const PH: usize>(
    chunk: &mut [u16],
    first_pair: usize,
    width: usize,
    planes: &[Plane; CHANNELS],
    curve: &[u16],
) {
    // Positions of [r, g1, g2, b] in the top row and the bottom row of the 2x2 cell.
    const TOP: [[usize; 2]; 4] = [[0, 1], [1, 0], [1, 3], [3, 1]];
    const BOTTOM: [[usize; 2]; 4] = [[2, 3], [3, 2], [0, 2], [2, 0]];
    let cw = width / 2;
    let curve: &[u16; 4096] = curve.try_into().expect("4096-entry curve");
    for (k, rows) in chunk.chunks_exact_mut(2 * width).enumerate() {
        let y = first_pair + k;
        fn row(p: &Plane, y: usize, cw: usize) -> &[i16] {
            &p.data[y * p.stride..y * p.stride + cw]
        }
        let (g, rg, bg, gd) =
            (row(&planes[0], y, cw), row(&planes[1], y, cw), row(&planes[2], y, cw), row(&planes[3], y, cw));
        let (top, bottom) = rows.split_at_mut(width);
        for x in 0..cw {
            let gs = g[x] as i32;
            let r = gs + 2 * (rg[x] as i32 - 2048);
            let b = gs + 2 * (bg[x] as i32 - 2048);
            let d = gd[x] as i32 - 2048;
            let v = [r, gs + d, gs - d, b].map(|v| curve[v.clamp(0, 4095) as usize]);
            top[2 * x] = v[TOP[PH][0]];
            top[2 * x + 1] = v[TOP[PH][1]];
            bottom[2 * x] = v[BOTTOM[PH][0]];
            bottom[2 * x + 1] = v[BOTTOM[PH][1]];
        }
    }
}

// ------------------------------------------------------------------------------------------
// Entry point
// ------------------------------------------------------------------------------------------

/// Decode one VC-5 bitstream (beginning at its four-byte start marker) into a `width x height`
/// Bayer mosaic in `out` (row-major, `width` samples per row).
///
/// `white_level` fixes the output bit depth (its bit length). `parallel` allows worker threads.
/// Errors: `CorruptFile` for malformed streams or unsupported parameters, `InvalidArgument`
/// when `out` holds fewer than `width * height` samples.
pub fn decode(
    data: &[u8],
    width: usize,
    height: usize,
    white_level: u32,
    phase: CfaPhase,
    out: &mut [u16],
    parallel: bool,
) -> Result<()> {
    if width == 0 || height == 0 || width % 2 != 0 || height % 2 != 0 || width > 65535 || height > 65535 {
        return Err(bad("VC-5: invalid image size"));
    }
    let samples = width * height;
    if out.len() < samples {
        return Err(Error::InvalidArgument("VC-5: output buffer too small".into()));
    }
    if white_level == 0 || white_level > 65535 {
        return Err(bad("VC-5: invalid white level"));
    }
    let out_bits = 32 - white_level.leading_zeros();
    let (cw, ch) = (width / 2, height / 2);
    let sizes = level_sizes(cw, ch);
    if sizes[LEVELS - 1].0 < 3 || sizes[LEVELS - 1].1 < 3 {
        return Err(bad("VC-5: image too small"));
    }
    let layout = Layout::parse(data, width, height, &sizes)?;
    let codec = codec();

    // Decode all bands, the biggest first.
    let mut jobs = Vec::with_capacity(CHANNELS * SUBBANDS);
    for s in (0..SUBBANDS).rev() {
        for c in 0..CHANNELS {
            if let Some(b) = layout.blocks[c][s] {
                jobs.push((c, s, b));
            }
        }
    }
    let decoded = par::map(jobs, parallel, |(c, s, b)| {
        let (w, h) = sizes[subband_level(s)];
        let r = if s == 0 { Ok(decode_lowpass(b, w * h)) } else { decode_highpass(b, w * h, codec) };
        (c, s, r)
    });
    let mut bands: Vec<Vec<Vec<i16>>> = (0..CHANNELS).map(|_| vec![Vec::new(); SUBBANDS]).collect();
    for (c, s, r) in decoded {
        bands[c][s] = r?;
    }

    let planes: Vec<Plane> = bands
        .into_iter()
        .enumerate()
        .map(|(c, b)| inverse_channel(b, &sizes, ch, layout.prescale[c], parallel))
        .collect();
    let planes: [Plane; CHANNELS] = planes.try_into().map_err(|_| bad("VC-5: channel count"))?;

    let shift = 16 - out_bits;
    let curve: Vec<u16> = codec.curve.iter().map(|&v| v >> shift).collect();
    let run = |chunk: &mut [u16], first_row: usize| {
        let pair = first_row / 2;
        match phase {
            CfaPhase::Rggb => mosaic_rows::<0>(chunk, pair, width, &planes, &curve),
            CfaPhase::Grbg => mosaic_rows::<1>(chunk, pair, width, &planes, &curve),
            CfaPhase::Gbrg => mosaic_rows::<2>(chunk, pair, width, &planes, &curve),
            CfaPhase::Bggr => mosaic_rows::<3>(chunk, pair, width, &planes, &curve),
        }
    };
    par::for_each_row_chunk(&mut out[..samples], width, 32, parallel, |first, chunk| run(chunk, first));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fnv(values: impl Iterator<Item = u16>) -> u64 {
        let mut h = 0xcbf2_9ce4_8422_2325u64;
        for v in values {
            for b in v.to_le_bytes() {
                h = (h ^ b as u64).wrapping_mul(0x100_0000_01b3);
            }
        }
        h
    }

    #[test]
    fn codebook_is_a_complete_prefix_code() {
        let mut words: Vec<(u32, u32)> = CODEBOOK.iter().map(|&(l, c, _, _)| (l as u32, c)).collect();
        words.push(END_MARKER);
        assert_eq!(words.len(), 264);
        let kraft: u64 = words.iter().map(|&(l, _)| 1u64 << (26 - l)).sum();
        assert_eq!(kraft, 1 << 26, "complete code");
        assert!(words.iter().all(|&(l, _)| (1..=26).contains(&l)));
        for (i, &(la, ca)) in words.iter().enumerate() {
            for &(lb, cb) in &words[i + 1..] {
                let l = la.min(lb);
                assert_ne!(ca >> (la - l), cb >> (lb - l), "prefix-free");
            }
        }
        let mut seen = [0u8; 256];
        let mut runs = Vec::new();
        for &(_, _, run, mag) in CODEBOOK.iter() {
            if mag > 0 {
                assert_eq!(run, 1);
                seen[mag as usize] += 1;
            } else {
                runs.push(run);
            }
        }
        assert!(seen[1..].iter().all(|&n| n == 1));
        runs.sort_unstable();
        assert_eq!(runs, [1, 12, 20, 32, 60, 100, 180, 320]);
    }

    #[test]
    fn inverse_companding_values() {
        let got: Vec<u32> = [1, 2, 3, 10, 50, 100, 128, 200, 254, 255].iter().map(|&m| expand(m)).collect();
        assert_eq!(got, [1, 2, 3, 10, 55, 146, 225, 570, 1013, 1023]);
        assert_eq!(expand(0), 0);
    }

    #[test]
    fn log_curve_golden_values() {
        let c = &codec().curve;
        let at = |bits: u32| [0, 1, 2, 100, 1000, 2047, 2048, 3000, 4094, 4095].map(|i| c[i] >> (16 - bits));
        assert_eq!(at(12), [0, 0, 0, 4, 79, 351, 352, 1130, 4091, 4095]);
        assert_eq!(at(14), [0, 0, 0, 17, 317, 1407, 1409, 4523, 16364, 16383]);
        assert_eq!(at(16), [0, 0, 1, 71, 1271, 5631, 5638, 18093, 65458, 65535]);
        assert_eq!(at(8), [0, 0, 0, 0, 4, 21, 22, 70, 255, 255]);
        assert_eq!(at(10), [0, 0, 0, 1, 19, 87, 88, 282, 1022, 1023]);
        let hash = |bits: u32| fnv(c.iter().map(|&v| v >> (16 - bits)));
        assert_eq!(hash(12), 0x45f4d6bc8d03a1c8);
        assert_eq!(hash(14), 0x25580d574e9070f6);
        assert_eq!(hash(16), 0xbdb7f58182cf3d82);
        assert_eq!(hash(8), 0x41524deb26d7c1b1);
        assert_eq!(hash(10), 0xffd0e7317f8568a0);
    }

    #[test]
    fn flat_lowpass_gives_flat_output() {
        let ll = Plane { data: vec![100; 9], stride: 3 };
        let z = vec![0i16; 9];
        let b = LevelBands { w: 3, h: 3, lh: &z, hl: &z, hh: &z };
        let out = inverse_level(&ll, &b, 0, false, 6, false);
        assert_eq!(out.stride, 6);
        // Each of the two passes (vertical, horizontal) halves a flat input.
        assert!(out.data.iter().all(|&v| v == 25));
    }

    #[test]
    fn prescale_is_fused_with_the_halving() {
        let mut out = [0i16];
        synth::<2, false>(&mut out, &[0], &[0], &[0], &[37], [1, 8, -1], 1);
        assert_eq!(out[0], 74);
        synth::<0, false>(&mut out, &[0], &[0], &[0], &[37], [1, 8, -1], 1);
        assert_eq!(out[0], 18);
        assert_eq!([0x2800u16; 3].map(|w| (0..3).map(|l| prescale_shift(w, l)).collect::<Vec<_>>()), [[0, 2, 2]; 3]);
        // The finest level clamps to 14 bits.
        synth::<0, true>(&mut out, &[0], &[0], &[0], &[30000], [1, 8, -1], 1);
        assert_eq!(out[0], 15000);
        synth::<2, true>(&mut out, &[0], &[0], &[0], &[30000], [1, 8, -1], 1);
        assert_eq!(out[0], 16383);
        synth::<0, true>(&mut out, &[0], &[0], &[0], &[-30000], [1, 8, -1], 1);
        assert_eq!(out[0], 0);
    }

    #[derive(Default)]
    struct Bits(Vec<u8>, u32);
    impl Bits {
        fn put(&mut self, v: u32, n: u32) {
            for i in (0..n).rev() {
                if self.1 % 8 == 0 {
                    self.0.push(0);
                }
                *self.0.last_mut().unwrap() |= (((v >> i) & 1) as u8) << (7 - self.1 % 8);
                self.1 += 1;
            }
        }
        fn finish(mut self, unit: usize) -> Vec<u8> {
            while self.0.len() % unit != 0 || self.0.is_empty() {
                self.0.push(0);
            }
            self.0
        }
    }

    fn tag(s: &mut Vec<u8>, t: i16, v: u16) {
        s.extend_from_slice(&t.to_be_bytes());
        s.extend_from_slice(&v.to_be_bytes());
    }

    /// A 34x34 stream with constant lowpass bands (value `low`) and all-zero highpass bands.
    fn minimal_stream(low: u16) -> Vec<u8> {
        let mut s = b"VC-5".to_vec();
        for (t, v) in [(0x0c, 4), (0x14, 34), (0x15, 34), (0x54, 4), (0x6a, 2), (0x6b, 2), (0x6c, 1), (0x66, 12)] {
            tag(&mut s, t, v);
        }
        let d = level_sizes(17, 17);
        for ch in 0..4u16 {
            tag(&mut s, 0x3e, ch);
            for sb in 0..SUBBANDS {
                tag(&mut s, 0x30, sb as u16);
                let (w, h) = d[subband_level(sb)];
                let payload = if sb == 0 {
                    tag(&mut s, 0x23, 16);
                    let mut b = Bits::default();
                    (0..w * h).for_each(|_| b.put(low as u32, 16));
                    b.finish(8)
                } else {
                    tag(&mut s, 0x35, 1);
                    let mut b = Bits::default();
                    (0..w * h).for_each(|_| b.put(0, 1));
                    b.put(END_MARKER.1, END_MARKER.0);
                    b.finish(4)
                };
                tag(&mut s, 0x6000, (payload.len() / 4) as u16);
                s.extend_from_slice(&payload);
            }
        }
        s.extend_from_slice(&[0xAA; 12]); // trailing bytes are ignored
        s
    }

    #[test]
    fn minimal_stream_decodes_to_a_constant_mosaic() {
        // Three levels of two halving passes: 16384 -> 256 in every channel, so G = 256 and the
        // differences are -1792: r, b and g1 clamp to 0 and g2 = 256 + 1792 = 2048.
        let s = minimal_stream(16384);
        // (x, y) of g2 inside the 2x2 block per phase.
        for (phase, g2) in [(CfaPhase::Rggb, 2), (CfaPhase::Grbg, 3), (CfaPhase::Gbrg, 3), (CfaPhase::Bggr, 2)] {
            let mut out = vec![0u16; 34 * 34 + 3];
            decode(&s, 34, 34, 4095, phase, &mut out, false).unwrap();
            for (i, &v) in out[..34 * 34].iter().enumerate() {
                let cell = (i / 34 % 2) * 2 + i % 2;
                assert_eq!(v, if cell == g2 { 352 } else { 0 }, "T[2048] at 12 bits, sample {i}");
            }
            assert_eq!(&out[34 * 34..], [0, 0, 0], "samples beyond the image stay untouched");
        }
        let mut out = vec![0u16; 34 * 34];
        decode(&s, 34, 34, 65535, CfaPhase::Rggb, &mut out, true).unwrap();
        assert_eq!(out[34], 5638);
        // A truncated stream fails.
        assert!(decode(&s[..s.len() / 2], 34, 34, 4095, CfaPhase::Rggb, &mut out, false).is_err());
    }

    #[test]
    fn argument_validation_order() {
        let s = minimal_stream(100);
        let mut out = vec![0u16; 34 * 34];
        let kind = |r: Result<()>| r.unwrap_err().kind();
        assert_eq!(kind(decode(&s, 0, 34, 4095, CfaPhase::Rggb, &mut out, false)), "CorruptFile");
        assert_eq!(kind(decode(&s, 35, 34, 4095, CfaPhase::Rggb, &mut out, false)), "CorruptFile");
        assert_eq!(kind(decode(&s, 65536, 34, 4095, CfaPhase::Rggb, &mut out, false)), "CorruptFile");
        // The buffer check comes after the size check and before the white level check.
        assert_eq!(kind(decode(&s, 34, 34, 0, CfaPhase::Rggb, &mut out[..10], false)), "InvalidArgument");
        assert_eq!(kind(decode(&s, 35, 34, 0, CfaPhase::Rggb, &mut out[..10], false)), "CorruptFile");
        assert_eq!(kind(decode(&s, 34, 34, 0, CfaPhase::Rggb, &mut out, false)), "CorruptFile");
        assert_eq!(kind(decode(&s, 34, 34, 65536, CfaPhase::Rggb, &mut out, false)), "CorruptFile");
        // Too small for three wavelet levels.
        assert_eq!(kind(decode(&s, 32, 32, 4095, CfaPhase::Rggb, &mut out, false)), "CorruptFile");
    }
}
