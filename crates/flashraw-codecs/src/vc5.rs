//! SMPTE ST 2073 (VC-5) wavelet codec as used by GoPro `.GPR` files (DNG compression 9).
//!
//! A GPR tile is one VC-5 bitstream ("VC-5" magic, big-endian 16-bit tag/value
//! pairs) describing a Bayer image as four half-resolution channels (G sum, R−G,
//! B−G, G difference). Each channel is a 3-level 2-D wavelet: one 16-bit lowpass
//! band for the coarsest level plus three run-length/VLC coded, quantised and
//! companded highpass bands per level. Reconstruction runs the vertical then
//! horizontal inverse transform per level (with the per-level prescale), clamps the
//! finest level to 14 bits, then recombines the four channels into the Bayer mosaic
//! through the VC-5 inverse-log curve rescaled to the DNG white level.
//!
//! The 264-entry run/value code book (table 17 of the standard) is GoPro's
//! published table (`gopro/gpr`, Apache-2.0 OR MIT).
//!
//! Parallelism: the 40 coded bands decode concurrently, channels reconstruct
//! concurrently and the final recombination is row-parallel.

use crate::x3f::PrefixCode;
use flashraw_core::bits::{BitPumpMsb, BitRead};
use flashraw_core::{Error, Result};

/// Bayer phase of the output mosaic (colours of the top-left 2x2 block).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CfaPhase {
    Rggb,
    Grbg,
    Gbrg,
    Bggr,
}

impl CfaPhase {
    /// The phase of a 2x2 Bayer pattern anchored at the image origin (None for other CFAs).
    pub fn from_pattern(p: &flashraw_core::CfaPattern) -> Option<Self> {
        use flashraw_core::CfaColor::{Blue, Green, Red};
        if !p.is_bayer() {
            return None;
        }
        Some(match (p.color_at(0, 0), p.color_at(1, 0)) {
            (Red, Green) => CfaPhase::Rggb,
            (Green, Red) => CfaPhase::Grbg,
            (Green, Blue) => CfaPhase::Gbrg,
            (Blue, Green) => CfaPhase::Bggr,
            _ => return None,
        })
    }
}

const NUM_CHANNELS: usize = 4;
const NUM_LEVELS: usize = 3;
const NUM_SUBBANDS: usize = 1 + 3 * NUM_LEVELS;
const RUN_BITS: u32 = 9;
const LOG_TABLE_BITS: u32 = 12;
const MARKER_BAND_END: i16 = 1;

/// Table 17: `(code length, code bits, run count, magnitude)`.
#[rustfmt::skip]
const TABLE17: [(u8, u32, u16, u16); 264] = [
    (1, 0x0, 1, 0), (2, 0x2, 1, 1), (3, 0x7, 1, 2), (5, 0x19, 1, 3), (6, 0x30, 1, 4), (6, 0x36, 1, 5),
    (7, 0x6F, 1, 8), (7, 0x63, 1, 6), (7, 0x69, 12, 0), (7, 0x6B, 1, 7), (8, 0xD1, 20, 0), (8, 0xD4, 1, 9),
    (8, 0xDC, 1, 10), (9, 0x189, 1, 11), (9, 0x18A, 32, 0), (9, 0x1A0, 1, 12), (9, 0x1AB, 1, 13), (10, 0x377, 1, 18),
    (10, 0x310, 1, 14), (10, 0x316, 1, 15), (10, 0x343, 60, 0), (10, 0x354, 1, 16), (10, 0x375, 1, 17),
    (11, 0x623, 1, 19), (11, 0x684, 1, 20), (11, 0x685, 100, 0), (11, 0x6AB, 1, 21), (11, 0x6EC, 1, 22),
    (12, 0xDDB, 1, 29), (12, 0xC5C, 1, 24), (12, 0xC5E, 1, 25), (12, 0xC44, 1, 23), (12, 0xD55, 1, 26),
    (12, 0xDD1, 1, 27), (12, 0xDD3, 1, 28), (13, 0x1BB5, 1, 35), (13, 0x188B, 1, 30), (13, 0x18BB, 1, 31),
    (13, 0x18BF, 180, 0), (13, 0x1AA8, 1, 32), (13, 0x1BA0, 1, 33), (13, 0x1BA5, 320, 0), (13, 0x1BA4, 1, 34),
    (14, 0x3115, 1, 36), (14, 0x3175, 1, 37), (14, 0x317D, 1, 38), (14, 0x3553, 1, 39), (14, 0x3768, 1, 40),
    (15, 0x6E87, 1, 46), (15, 0x6ED3, 1, 47), (15, 0x62E8, 1, 42), (15, 0x62F8, 1, 43), (15, 0x6228, 1, 41),
    (15, 0x6AA4, 1, 44), (15, 0x6E85, 1, 45), (16, 0xC453, 1, 48), (16, 0xC5D3, 1, 49), (16, 0xC5F3, 1, 50),
    (16, 0xDDA4, 1, 53), (16, 0xDD08, 1, 51), (16, 0xDD0C, 1, 52), (17, 0x1BB4B, 1, 61), (17, 0x1BB4A, 1, 60),
    (17, 0x18BA5, 1, 55), (17, 0x18BE5, 1, 56), (17, 0x1AA95, 1, 57), (17, 0x1AA97, 1, 58), (17, 0x188A4, 1, 54),
    (17, 0x1BA13, 1, 59), (18, 0x31748, 1, 62), (18, 0x317C8, 1, 63), (18, 0x35528, 1, 64), (18, 0x3552C, 1, 65),
    (18, 0x37424, 1, 66), (18, 0x37434, 1, 67), (18, 0x37436, 1, 68), (19, 0x62294, 1, 69), (19, 0x62E92, 1, 70),
    (19, 0x62F92, 1, 71), (19, 0x6AA52, 1, 72), (19, 0x6AA5A, 1, 73), (19, 0x6E86A, 1, 75), (19, 0x6E86E, 1, 76),
    (19, 0x6E84A, 1, 74), (20, 0xC452A, 1, 77), (20, 0xC5D27, 1, 78), (20, 0xC5F26, 1, 79), (20, 0xD54A6, 1, 80),
    (20, 0xD54B6, 1, 81), (20, 0xDD096, 1, 82), (20, 0xDD0D6, 1, 83), (20, 0xDD0DE, 1, 84), (21, 0x188A56, 1, 85),
    (21, 0x18BA4D, 1, 86), (21, 0x18BE4E, 1, 87), (21, 0x18BE4F, 1, 88), (21, 0x1AA96E, 1, 89),
    (21, 0x1BA12E, 1, 90), (21, 0x1BA12F, 1, 91), (21, 0x1BA1AF, 1, 92), (21, 0x1BA1BF, 1, 93),
    (22, 0x37435D, 1, 99), (22, 0x37437D, 1, 100), (22, 0x317498, 1, 94), (22, 0x35529C, 1, 95),
    (22, 0x35529D, 1, 96), (22, 0x3552DE, 1, 97), (22, 0x3552DF, 1, 98), (23, 0x62E933, 1, 102),
    (23, 0x62295D, 1, 101), (23, 0x6AA53D, 1, 103), (23, 0x6AA53F, 1, 105), (23, 0x6AA53E, 1, 104),
    (23, 0x6E86B9, 1, 106), (23, 0x6E86F8, 1, 107), (24, 0xD54A79, 1, 111), (24, 0xC5D265, 1, 109),
    (24, 0xC452B8, 1, 108), (24, 0xDD0D71, 1, 113), (24, 0xD54A78, 1, 110), (24, 0xDD0D70, 1, 112),
    (24, 0xDD0DF2, 1, 114), (24, 0xDD0DF3, 1, 115), (25, 0x188A5F6, 1, 225), (25, 0x188A5F5, 1, 189),
    (25, 0x188A5F4, 1, 188), (25, 0x188A5F3, 1, 203), (25, 0x188A5F2, 1, 202), (25, 0x188A5F1, 1, 197),
    (25, 0x188A5F0, 1, 207), (25, 0x188A5EF, 1, 169), (25, 0x188A5EE, 1, 223), (25, 0x188A5ED, 1, 159),
    (25, 0x188A5AA, 1, 235), (25, 0x188A5E3, 1, 152), (25, 0x188A5DF, 1, 192), (25, 0x188A589, 1, 179),
    (25, 0x188A5DD, 1, 201), (25, 0x188A578, 1, 172), (25, 0x188A5E0, 1, 149), (25, 0x188A588, 1, 178),
    (25, 0x188A5D6, 1, 120), (25, 0x188A5DB, 1, 219), (25, 0x188A5E1, 1, 150), (25, 0x188A587, 1, 127),
    (25, 0x188A59A, 1, 211), (25, 0x188A5C4, 1, 125), (25, 0x188A5EC, 1, 158), (25, 0x188A586, 1, 247),
    (25, 0x188A573, 1, 238), (25, 0x188A59C, 1, 163), (25, 0x188A5C8, 1, 228), (25, 0x188A5FB, 1, 183),
    (25, 0x188A5A1, 1, 217), (25, 0x188A5EB, 1, 168), (25, 0x188A5A8, 1, 122), (25, 0x188A584, 1, 128),
    (25, 0x188A5D2, 1, 249), (25, 0x188A599, 1, 187), (25, 0x188A598, 1, 186), (25, 0x188A583, 1, 136),
    (25, 0x18BA4C9, 1, 181), (25, 0x188A5D0, 1, 255), (25, 0x188A594, 1, 230), (25, 0x188A582, 1, 135),
    (25, 0x188A5CB, 1, 233), (25, 0x188A5D8, 1, 222), (25, 0x188A5E7, 1, 145), (25, 0x188A581, 1, 134),
    (25, 0x188A5EA, 1, 167), (25, 0x188A5A9, 1, 248), (25, 0x188A5A6, 1, 209), (25, 0x188A580, 1, 243),
    (25, 0x188A5A0, 1, 216), (25, 0x188A59D, 1, 164), (25, 0x188A5C3, 1, 140), (25, 0x188A57F, 1, 157),
    (25, 0x188A5C0, 1, 239), (25, 0x188A5DE, 1, 191), (25, 0x188A5D4, 1, 251), (25, 0x188A57E, 1, 156),
    (25, 0x188A5C2, 1, 139), (25, 0x188A592, 1, 242), (25, 0x188A5CD, 1, 133), (25, 0x188A57D, 1, 162),
    (25, 0x188A5A3, 1, 213), (25, 0x188A5E8, 1, 165), (25, 0x188A5A2, 1, 212), (25, 0x188A57C, 1, 227),
    (25, 0x188A58E, 1, 198), (25, 0x188A5B3, 1, 236), (25, 0x188A5B2, 1, 234), (25, 0x188A5B1, 1, 117),
    (25, 0x188A5B0, 1, 215), (25, 0x188A5AF, 1, 124), (25, 0x188A5AE, 1, 123), (25, 0x188A5AD, 1, 254),
    (25, 0x188A5AC, 1, 253), (25, 0x188A5AB, 1, 148), (25, 0x188A5DA, 1, 218), (25, 0x188A5E4, 1, 146),
    (25, 0x188A5E5, 1, 147), (25, 0x188A5D9, 1, 224), (25, 0x188A5B5, 1, 143), (25, 0x188A5BC, 1, 184),
    (25, 0x188A5BD, 1, 185), (25, 0x188A5E9, 1, 166), (25, 0x188A5CC, 1, 132), (25, 0x188A585, 1, 129),
    (25, 0x188A5D3, 1, 250), (25, 0x188A5E2, 1, 151), (25, 0x188A595, 1, 119), (25, 0x188A596, 1, 193),
    (25, 0x188A5B8, 1, 176), (25, 0x188A590, 1, 245), (25, 0x188A5C9, 1, 229), (25, 0x188A5A4, 1, 206),
    (25, 0x188A5E6, 1, 144), (25, 0x188A5A5, 1, 208), (25, 0x188A5CE, 1, 137), (25, 0x188A5BF, 1, 241),
    (25, 0x188A572, 1, 237), (25, 0x188A59B, 1, 190), (25, 0x188A5BE, 1, 240), (25, 0x188A5C7, 1, 131),
    (25, 0x188A5CA, 1, 232), (25, 0x188A5D5, 1, 252), (25, 0x188A57B, 1, 171), (25, 0x188A58D, 1, 205),
    (25, 0x188A58C, 1, 204), (25, 0x188A58B, 1, 118), (25, 0x188A58A, 1, 214), (25, 0x18BA4C8, 1, 180),
    (25, 0x188A5C5, 1, 126), (25, 0x188A5FA, 1, 182), (25, 0x188A5BB, 1, 175), (25, 0x188A5C1, 1, 141),
    (25, 0x188A5CF, 1, 138), (25, 0x188A5B9, 1, 177), (25, 0x188A5B6, 1, 153), (25, 0x188A597, 1, 194),
    (25, 0x188A5FE, 1, 160), (25, 0x188A5D7, 1, 121), (25, 0x188A5BA, 1, 174), (25, 0x188A591, 1, 246),
    (25, 0x188A5C6, 1, 130), (25, 0x188A5DC, 1, 200), (25, 0x188A57A, 1, 170), (25, 0x188A59F, 1, 221),
    (25, 0x188A5F9, 1, 196), (25, 0x188A5B4, 1, 142), (25, 0x188A5A7, 1, 210), (25, 0x188A58F, 1, 199),
    (25, 0x188A5FD, 1, 155), (25, 0x188A5B7, 1, 154), (25, 0x188A593, 1, 244), (25, 0x188A59E, 1, 220),
    (25, 0x188A5F8, 1, 195), (25, 0x188A5FF, 1, 161), (25, 0x188A5FC, 1, 231), (25, 0x188A579, 1, 173),
    (25, 0x188A5F7, 1, 226), (26, 0x3114BA2, 1, 116), (26, 0x3114BA3, 0, 1),
];

/// Invert the VC-5 companding curve.
fn decompand(v: i16) -> i16 {
    let c = v as f64;
    let c = c + (c * c * c * 768.0) / (255.0 * 255.0 * 255.0);
    c.clamp(i16::MIN as f64, i16::MAX as f64) as i16
}

/// The run/value code book; decoded values are `decompanded magnitude << 9 | run`.
fn code_book() -> Result<PrefixCode> {
    let codes: Vec<(u32, u32, u32)> = TABLE17
        .iter()
        .map(|&(len, bits, count, value)| {
            let v = ((decompand(value as i16) as u16 as u32) << RUN_BITS) | count as u32;
            (bits, len as u32, v)
        })
        .collect();
    PrefixCode::new(&codes)
}

/// The inverse-log output curve for a given output bit depth.
fn log_table(output_bits: u32) -> Vec<u16> {
    let n = 1usize << LOG_TABLE_BITS;
    (0..n)
        .map(|i| {
            let x = i as f64 / (n as f64 - 1.0);
            let y = (113f64.powf(x) - 1.0) / 112.0;
            let y = (u16::MAX as f64 * y) as u32;
            (y >> (16 - output_bits)) as u16
        })
        .collect()
}

/// A decoded/reconstructed band: `width x height` samples with row pitch `stride`.
struct Band {
    width: usize,
    height: usize,
    stride: usize,
    data: Vec<i16>,
}

impl Band {
    fn new(width: usize, height: usize) -> Result<Self> {
        let n = width.checked_mul(height).ok_or_else(|| Error::corrupt("VC-5 band size overflow"))?;
        Ok(Band { width, height, stride: width, data: vec![0; n] })
    }
    #[inline(always)]
    fn at(&self, row: usize, col: usize) -> i32 {
        self.data[row * self.stride + col] as i32
    }
}

#[derive(Clone, Copy)]
enum Coded<'a> {
    Lowpass { precision: u32, data: &'a [u8] },
    Highpass { quant: i16, data: &'a [u8] },
}

/// Parsed bitstream: per channel, per wavelet level (0 = finest), per band (0 = lowpass).
struct Stream<'a> {
    bands: [[[Option<Coded<'a>>; 4]; NUM_LEVELS]; NUM_CHANNELS],
    prescale: [[u16; NUM_LEVELS]; NUM_CHANNELS],
}

/// Band dimensions per level: index 0 is the channel size, `i` the bands of level `i`.
fn level_dims(width: usize, height: usize) -> [(usize, usize); NUM_LEVELS + 1] {
    let mut d = [(0, 0); NUM_LEVELS + 1];
    let (mut w, mut h) = (width, height);
    for e in d.iter_mut() {
        w = w.div_ceil(2);
        h = h.div_ceil(2);
        *e = (w, h);
    }
    d
}

fn parse(data: &[u8], width: usize, height: usize) -> Result<Stream<'_>> {
    if data.get(..4) != Some(b"VC-5") {
        return Err(Error::corrupt("missing VC-5 signature"));
    }
    let mut st = Stream { bands: Default::default(), prescale: [[0; NUM_LEVELS]; NUM_CHANNELS] };
    let dims = level_dims(width, height);
    let mut pos = 4usize;
    let mut channel = 0usize;
    let mut subband: Option<usize> = None;
    let mut precision: Option<u32> = None;
    let mut quant: Option<i16> = None;
    let mut complete = [false; NUM_CHANNELS];
    while !complete.iter().all(|&c| c) {
        let b = data.get(pos..pos + 4).ok_or_else(|| Error::corrupt("truncated VC-5 bitstream"))?;
        pos += 4;
        let mut tag = i16::from_be_bytes([b[0], b[1]]);
        let val = u16::from_be_bytes([b[2], b[3]]);
        let mut optional = tag < 0;
        if optional {
            tag = tag.wrapping_neg();
        }
        let expect = |ok: bool, what: &str| {
            if ok { Ok(()) } else { Err(Error::corrupt(format!("VC-5: unexpected {what} {val}"))) }
        };
        match tag {
            0x000c => expect(val as usize == NUM_CHANNELS, "channel count")?,
            0x0014 => expect(val as usize == width, "image width")?,
            0x0015 => expect(val as usize == height, "image height")?,
            0x000e => expect(val as usize == NUM_SUBBANDS, "subband count")?,
            0x0054 => expect(val == 4, "image format")?,
            0x0066 => expect(val as u32 == LOG_TABLE_BITS, "bits per component")?,
            0x006a | 0x006b => expect(val == 2, "pattern size")?,
            0x006c => expect(val == 1, "components per sample")?,
            0x0023 => {
                expect((8..=16).contains(&val), "lowpass precision")?;
                precision = Some(val as u32);
            }
            0x003e => {
                expect((val as usize) < NUM_CHANNELS, "channel number")?;
                channel = val as usize;
            }
            0x0030 => {
                expect((val as usize) < NUM_SUBBANDS, "subband number")?;
                subband = Some(val as usize);
            }
            0x0035 => quant = Some(val as i16),
            0x006d => {
                // Two bits per level in a 14-bit field, finest level in the top bits.
                for (i, p) in st.prescale[channel].iter_mut().enumerate() {
                    *p = (val >> (14 - 2 * i as u32)) & 3;
                }
            }
            _ => {
                let t = tag as u16;
                let mut size = 0usize;
                if t & 0x2000 != 0 {
                    size = (((t & 0xFF) as usize) << 16) | val as usize;
                } else if t & 0x4000 != 0 {
                    size = val as usize;
                }
                if t & 0x6000 == 0x6000 {
                    let len = size * 4;
                    let chunk = data
                        .get(pos..pos.saturating_add(len))
                        .ok_or_else(|| Error::corrupt("truncated VC-5 codeblock"))?;
                    pos += len;
                    let sb = subband.take().ok_or_else(|| Error::corrupt("VC-5 codeblock without subband"))?;
                    // Subband 0: lowpass of the coarsest level; then 3 highpass bands per
                    // level from coarse to fine.
                    let (level, band) =
                        if sb == 0 { (NUM_LEVELS - 1, 0) } else { (NUM_LEVELS - 1 - (sb - 1) / 3, 1 + (sb - 1) % 3) };
                    let slot = &mut st.bands[channel][level][band];
                    if slot.is_some() {
                        return Err(Error::corrupt("VC-5 band specified twice"));
                    }
                    *slot = Some(if sb == 0 {
                        let p = precision.take().ok_or_else(|| Error::corrupt("VC-5 lowpass without precision"))?;
                        let (w, h) = dims[NUM_LEVELS];
                        let bytes = ((w * h) as u64 * p as u64).div_ceil(64) as usize * 8;
                        let d = chunk.get(..bytes).ok_or_else(|| Error::corrupt("truncated VC-5 lowpass band"))?;
                        Coded::Lowpass { precision: p, data: d }
                    } else {
                        let q = quant.take().ok_or_else(|| Error::corrupt("VC-5 highpass without quantisation"))?;
                        Coded::Highpass { quant: q, data: chunk }
                    });
                    let bands = &st.bands[channel];
                    complete[channel] =
                        bands[NUM_LEVELS - 1][0].is_some() && bands.iter().all(|l| l[1..].iter().all(|b| b.is_some()));
                    continue;
                }
                if t & 0x2000 != 0 {
                    // Other large chunks are containers whose payload follows as tags.
                    optional = true;
                    size = 0;
                }
                if !optional {
                    return Err(Error::corrupt(format!("VC-5: unknown required tag 0x{t:04x}")));
                }
                pos = pos.saturating_add(size * 4);
            }
        }
    }
    Ok(st)
}

fn decode_lowpass(data: &[u8], precision: u32, w: usize, h: usize) -> Result<Band> {
    let mut band = Band::new(w, h)?;
    let mut bits = BitPumpMsb::new(data);
    for v in band.data.iter_mut() {
        *v = bits.get(precision) as i16;
    }
    Ok(band)
}

#[inline(always)]
fn read_rlv(code: &PrefixCode, bits: &mut BitPumpMsb) -> Result<(i16, usize)> {
    let v = code.decode(bits)?;
    let count = (v & ((1 << RUN_BITS) - 1)) as usize;
    let mut value = (v >> RUN_BITS) as u16 as i16;
    if value != 0 && bits.get(1) != 0 {
        value = -value;
    }
    Ok((value, count))
}

fn decode_highpass(code: &PrefixCode, data: &[u8], quant: i16, w: usize, h: usize) -> Result<Band> {
    let mut band = Band::new(w, h)?;
    let mut bits = BitPumpMsb::new(data);
    let out = &mut band.data[..];
    let mut i = 0usize;
    while i < out.len() {
        let (value, count) = read_rlv(code, &mut bits)?;
        if count == 0 {
            return Err(Error::corrupt("VC-5 end-of-band marker inside band"));
        }
        let value = value.checked_mul(quant).ok_or_else(|| Error::corrupt("VC-5 dequantisation overflow"))?;
        let end =
            i.checked_add(count).filter(|&e| e <= out.len()).ok_or_else(|| Error::corrupt("VC-5 run past band end"))?;
        out[i..end].fill(value);
        i = end;
        if bits.exhausted() {
            return Err(Error::corrupt("truncated VC-5 highpass band"));
        }
    }
    let (value, count) = read_rlv(code, &mut bits)?;
    if value != MARKER_BAND_END || count != 0 {
        return Err(Error::corrupt("missing VC-5 end-of-band marker"));
    }
    Ok(band)
}

/// Inverse filter taps: (even, odd) multipliers for (high, low[k+s], low[k+s+1], low[k+s+2])
/// and the start offset `s` at the first, interior and last position.
type Taps = ([i32; 4], [i32; 4], isize);
const FIRST: Taps = ([1, 11, -4, 1], [-1, 5, 4, -1], 0);
const MIDDLE: Taps = ([1, 1, 8, -1], [-1, -1, 8, 1], -1);
const LAST: Taps = ([1, -1, 4, 5], [-1, 1, -4, 11], -2);

#[inline(always)]
fn taps_for(i: usize, n: usize) -> Taps {
    if i == 0 {
        FIRST
    } else if i + 1 < n {
        MIDDLE
    } else {
        LAST
    }
}

#[inline(always)]
fn tap(m: &[i32; 4], high: i32, l: [i32; 3], descale: u32) -> i32 {
    let lows = (m[1] * l[0] + m[2] * l[1] + m[3] * l[2] + 4) >> 3;
    ((m[0] * high + lows) << descale) >> 1
}

fn check_pair(high: &Band, low: &Band) -> Result<()> {
    if high.width < 3 || high.height < 3 || low.width < high.width || low.height < high.height {
        return Err(Error::corrupt("VC-5 band geometry too small"));
    }
    Ok(())
}

/// Vertical inverse step: `high`/`low` (w x h) → (w x 2h).
fn vertical(high: &Band, low: &Band) -> Result<Band> {
    check_pair(high, low)?;
    let (w, h) = (high.width, high.height);
    let mut out = Band::new(w, 2 * h)?;
    for row in 0..h {
        let (even, odd, shift) = taps_for(row, h);
        let base = (row as isize + shift) as usize;
        let (top, bottom) = out.data[2 * row * w..(2 * row + 2) * w].split_at_mut(w);
        for col in 0..w {
            let l = [low.at(base, col), low.at(base + 1, col), low.at(base + 2, col)];
            let hv = high.at(row, col);
            top[col] = tap(&even, hv, l, 0) as i16;
            bottom[col] = tap(&odd, hv, l, 0) as i16;
        }
    }
    Ok(out)
}

/// Horizontal inverse step: `low`/`high` (w x h) → (2w x h), optionally clamped to 14 bits.
fn horizontal(low: &Band, high: &Band, descale: u32, clamp: bool) -> Result<Band> {
    check_pair(high, low)?;
    let (w, h) = (high.width, high.height);
    let mut out = Band::new(2 * w, h)?;
    let fin = |v: i32| if clamp { v.clamp(0, (1 << 14) - 1) as i16 } else { v as i16 };
    for row in 0..h {
        let o = &mut out.data[row * 2 * w..(row + 1) * 2 * w];
        for col in 0..w {
            let (even, odd, shift) = taps_for(col, w);
            let base = (col as isize + shift) as usize;
            let l = [low.at(row, base), low.at(row, base + 1), low.at(row, base + 2)];
            let hv = high.at(row, col);
            o[2 * col] = fin(tap(&even, hv, l, descale));
            o[2 * col + 1] = fin(tap(&odd, hv, l, descale));
        }
    }
    Ok(out)
}

/// Decode a VC-5 bitstream (starting at the "VC-5" signature) into a `width x height`
/// Bayer mosaic (`out`, row-major). `white_level` selects the output bit depth of the
/// inverse-log curve (e.g. 4095 → 12-bit samples).
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
        return Err(Error::corrupt(format!("invalid VC-5 image size {width}x{height}")));
    }
    if out.len() < width * height {
        return Err(Error::InvalidArgument("VC-5: output buffer too small".into()));
    }
    if white_level == 0 || white_level > 0xFFFF {
        return Err(Error::corrupt(format!("invalid VC-5 white level {white_level}")));
    }
    let output_bits = 32 - white_level.leading_zeros();
    let st = parse(data, width, height)?;
    let code = code_book()?;
    let dims = level_dims(width, height);

    // Decode every coded band (index = (channel * levels + level) * 4 + band).
    let coded: Vec<(usize, Coded)> = (0..NUM_CHANNELS * NUM_LEVELS * 4)
        .filter_map(|i| st.bands[i / (NUM_LEVELS * 4)][(i / 4) % NUM_LEVELS][i % 4].map(|c| (i, c)))
        .collect();
    let decoded = flashraw_core::par::map(coded, parallel, |(i, cb)| -> Result<(usize, Band)> {
        let (w, h) = dims[(i / 4) % NUM_LEVELS + 1];
        Ok((
            i,
            match cb {
                Coded::Lowpass { precision, data } => decode_lowpass(data, precision, w, h)?,
                Coded::Highpass { quant, data } => decode_highpass(&code, data, quant, w, h)?,
            },
        ))
    });
    let mut bands: Vec<Option<Band>> = (0..NUM_CHANNELS * NUM_LEVELS * 4).map(|_| None).collect();
    for r in decoded {
        let (i, b) = r?;
        bands[i] = Some(b);
    }

    // Reconstruct each channel from the coarsest level to the finest.
    let mut per_channel: Vec<(usize, Vec<Option<Band>>)> = Vec::with_capacity(NUM_CHANNELS);
    let mut it = bands.into_iter();
    for c in 0..NUM_CHANNELS {
        per_channel.push((c, it.by_ref().take(NUM_LEVELS * 4).collect()));
    }
    let prescale = st.prescale;
    let finals = flashraw_core::par::map(per_channel, parallel, |(c, mut v)| -> Result<Band> {
        let mut low = v[(NUM_LEVELS - 1) * 4].take().ok_or_else(|| Error::corrupt("missing VC-5 lowpass band"))?;
        for level in (0..NUM_LEVELS).rev() {
            let mut take = |b: usize| v[level * 4 + b].take().ok_or_else(|| Error::corrupt("missing VC-5 band"));
            let (lh, hl, hh) = (take(1)?, take(2)?, take(3)?);
            let lowpass = vertical(&hl, &low)?;
            let highpass = vertical(&hh, &lh)?;
            let descale = if prescale[c][level] == 2 { 2 } else { 0 };
            low = horizontal(&lowpass, &highpass, descale, level == 0)?;
        }
        Ok(low)
    });
    let mut ch = Vec::with_capacity(NUM_CHANNELS);
    for f in finals {
        ch.push(f?);
    }
    let (hw, hh) = (width / 2, height / 2);
    if ch.iter().any(|b| b.width < hw || b.height < hh) {
        return Err(Error::corrupt("VC-5 reconstructed channel too small"));
    }

    // Recombine into the mosaic through the output curve.
    let lut = log_table(output_bits);
    let curve = |v: i32| lut[v.clamp(0, (1 << LOG_TABLE_BITS) - 1) as usize];
    flashraw_core::par::for_each_row_chunk(&mut out[..width * height], width * 2, 16, parallel, |first, chunk| {
        for (i, rows) in chunk.chunks_exact_mut(width * 2).enumerate() {
            let row = first + i;
            let (r0, r1) = rows.split_at_mut(width);
            for col in 0..hw {
                let gs = ch[0].at(row, col);
                let rg = ch[1].at(row, col) - 2048;
                let bg = ch[2].at(row, col) - 2048;
                let gd = ch[3].at(row, col) - 2048;
                let (r, b, g1, g2) = (curve(gs + 2 * rg), curve(gs + 2 * bg), curve(gs + gd), curve(gs - gd));
                // The first green of the coded pattern stays first when the phase swaps rows.
                let q = match phase {
                    CfaPhase::Rggb => [r, g1, g2, b],
                    CfaPhase::Grbg => [g1, r, b, g2],
                    CfaPhase::Gbrg => [g1, b, r, g2],
                    CfaPhase::Bggr => [b, g1, g2, r],
                };
                r0[2 * col] = q[0];
                r0[2 * col + 1] = q[1];
                r1[2 * col] = q[2];
                r1[2 * col + 1] = q[3];
            }
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decompand_is_odd_and_monotonic() {
        let mut prev = i16::MIN;
        for v in 0..=255i16 {
            assert_eq!(decompand(-v), -decompand(v));
            assert!(decompand(v) >= prev);
            prev = decompand(v);
        }
        assert_eq!(decompand(1), 1);
        assert_eq!(decompand(255), 1023);
    }

    #[test]
    fn code_book_round_trips() {
        let code = code_book().unwrap();
        for &(len, bits, count, value) in TABLE17.iter() {
            let buf = ((bits as u64) << (64 - len as u32)).to_be_bytes();
            let mut b = BitPumpMsb::new(&buf);
            let got = code.decode(&mut b).unwrap();
            assert_eq!(got & 0x1FF, count as u32);
            assert_eq!((got >> 9) as u16 as i16, decompand(value as i16));
        }
    }

    #[test]
    fn log_curve_endpoints() {
        let t = log_table(12);
        assert_eq!(t[0], 0);
        assert_eq!(t[4095], 4095);
        assert!(t.windows(2).all(|w| w[0] <= w[1]));
        assert_eq!(log_table(14)[4095], 16383);
    }

    #[test]
    fn inverse_transform_of_flat_lowpass_is_flat() {
        let mut low = Band::new(4, 4).unwrap();
        low.data.fill(100);
        let zero = Band::new(4, 4).unwrap();
        let v = vertical(&zero, &low).unwrap();
        assert!(v.data.iter().all(|&x| x == 50));
        let lp = Band { width: 4, height: 8, stride: 4, data: vec![100; 32] };
        let hz = Band::new(4, 8).unwrap();
        let h = horizontal(&lp, &hz, 0, true).unwrap();
        assert_eq!((h.width, h.height), (8, 8));
        assert!(h.data.iter().all(|&x| x == 50));
    }

    #[test]
    fn malformed_streams_error() {
        let mut out = vec![0u16; 64 * 64];
        assert!(decode(b"nope", 64, 64, 4095, CfaPhase::Rggb, &mut out, false).is_err());
        assert!(decode(b"VC-5", 64, 64, 4095, CfaPhase::Rggb, &mut out, false).is_err());
        let mut s = b"VC-5".to_vec();
        s.extend_from_slice(&[0x00, 0x0c, 0x00, 0x04, 0x00, 0x14, 0x00, 0x40, 0x00, 0x15, 0x00, 0x41]);
        assert!(decode(&s, 64, 64, 4095, CfaPhase::Rggb, &mut out, false).is_err());
        let mut s = b"VC-5".to_vec();
        s.extend_from_slice(&[0x00, 0x77, 0x00, 0x00]);
        assert!(decode(&s, 64, 64, 4095, CfaPhase::Rggb, &mut out, false).is_err());
        assert!(decode(b"VC-5", 63, 64, 4095, CfaPhase::Rggb, &mut out, false).is_err());
        assert!(decode(b"VC-5", 64, 64, 0, CfaPhase::Rggb, &mut out, false).is_err());
    }
}
