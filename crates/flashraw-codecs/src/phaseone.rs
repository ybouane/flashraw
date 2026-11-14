//! Phase One IIQ entropy coders (IIQ L, IIQ S) and the sample unscrambler of the keyed
//! uncompressed payloads.
//!
//! IIQ keeps a per-row offset table, so each row of a compressed file is an independent
//! bit stream. Bits are read most significant first. Both coders keep two interleaved
//! channels (even and odd columns) and work in blocks of eight columns.
//!
//! * IIQ L: at the start of each block a short prefix code per channel selects the bit
//!   width of the signed differences stored for that channel (or repeats the previous
//!   width); width 14 is the escape for a 16-bit literal. A trailing partial block is
//!   made of 16-bit literals.
//! * IIQ S: a 16-bit row prologue gives the base precision; each block carries a level
//!   update per channel and one correction code, followed by eight quantised residuals.
//!   Level 9 stores 14-bit literals.

use flashraw_core::bits::BitRead;
use flashraw_core::{Error, Result};

/// Variant parameters of the IIQ L coder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IiqLParams {
    /// Left shift applied to every decoded sample (2 for 14-bit data, 0 for 16-bit data).
    pub shift: u32,
    /// Samples below 256 are first mapped through the low-range curve.
    pub low_curve: bool,
}

/// A prefix code as a list of codewords `(length in bits, codeword, value)`, expanded into a
/// lookup table over the next `N` bits (`N` = the longest codeword): every table slot whose leading
/// bits equal a codeword holds `(codeword length, value)`.
const fn expand_prefix_code<const N: usize, const M: usize>(words: &[(u8, u8, u8); M], bits: u32) -> [(u8, u8); N] {
    let mut table = [(0u8, 0u8); N];
    let mut i = 0;
    while i < M {
        let (len, code, value) = words[i];
        let free = bits - len as u32;
        let first = (code as usize) << free;
        let mut k = 0;
        while k < (1usize << free) {
            table[first + k] = (len, value);
            k += 1;
        }
        i += 1;
    }
    table
}

/// Codewords of the IIQ L width selector: `1` repeats the previous width; a run of one to four
/// zeros then a one then one more bit picks one of two widths; five zeros and a bit pick between
/// the 16-bit escape (14) and 13. Value 0 stands for "repeat".
const L_CODEWORDS: [(u8, u8, u8); 11] = [
    (1, 0b1, 0),
    (3, 0b010, 8),
    (3, 0b011, 7),
    (4, 0b0010, 6),
    (4, 0b0011, 9),
    (5, 0b00010, 11),
    (5, 0b00011, 10),
    (6, 0b000010, 5),
    (6, 0b000011, 12),
    (6, 0b000000, 14),
    (6, 0b000001, 13),
];

/// Length code lookup on the next 6 bits: `(bits consumed, width)`, width 0 = repeat.
const L_CODE: [(u8, u8); 64] = expand_prefix_code(&L_CODEWORDS, 6);

/// Low-range curve of IIQ L format 5: `floor(i * i / 3.969 + 0.5)`, with `curve[255] = 16383`.
static LOW_CURVE: [u16; 256] = [
    0, 0, 1, 2, 4, 6, 9, 12, 16, 20, 25, 30, 36, 43, 49, 57, 64, 73, 82, 91, 101, 111, 122, 133, 145, 157, 170, 184,
    198, 212, 227, 242, 258, 274, 291, 309, 327, 345, 364, 383, 403, 424, 444, 466, 488, 510, 533, 557, 580, 605, 630,
    655, 681, 708, 735, 762, 790, 819, 848, 877, 907, 938, 969, 1000, 1032, 1064, 1098, 1131, 1165, 1200, 1235, 1270,
    1306, 1343, 1380, 1417, 1455, 1494, 1533, 1572, 1612, 1653, 1694, 1736, 1778, 1820, 1863, 1907, 1951, 1996, 2041,
    2086, 2133, 2179, 2226, 2274, 2322, 2371, 2420, 2469, 2520, 2570, 2621, 2673, 2725, 2778, 2831, 2885, 2939, 2993,
    3049, 3104, 3160, 3217, 3274, 3332, 3390, 3449, 3508, 3568, 3628, 3689, 3750, 3812, 3874, 3937, 4000, 4064, 4128,
    4193, 4258, 4324, 4390, 4457, 4524, 4592, 4660, 4729, 4798, 4868, 4938, 5009, 5080, 5152, 5224, 5297, 5371, 5444,
    5519, 5594, 5669, 5745, 5821, 5898, 5975, 6053, 6132, 6210, 6290, 6370, 6450, 6531, 6612, 6694, 6777, 6859, 6943,
    7027, 7111, 7196, 7281, 7367, 7454, 7541, 7628, 7716, 7804, 7893, 7983, 8073, 8163, 8254, 8346, 8438, 8530, 8623,
    8717, 8811, 8905, 9000, 9095, 9191, 9288, 9385, 9482, 9580, 9679, 9778, 9878, 9978, 10078, 10179, 10281, 10383,
    10485, 10588, 10692, 10796, 10900, 11006, 11111, 11217, 11324, 11431, 11538, 11647, 11755, 11864, 11974, 12084,
    12195, 12306, 12417, 12529, 12642, 12755, 12869, 12983, 13098, 13213, 13328, 13444, 13561, 13678, 13796, 13914,
    14033, 14152, 14272, 14392, 14512, 14634, 14755, 14878, 15000, 15123, 15247, 15371, 15496, 15621, 15747, 15873,
    16000, 16127, 16255, 16383,
];

/// Decode one IIQ L row of `out.len()` samples from a bit source positioned at the row start.
#[inline(always)]
pub fn decode_l_row<B: BitRead>(bits: &mut B, out: &mut [u16], p: IiqLParams) -> Result<()> {
    let width = out.len();
    let full = width & !7;
    let shift = p.shift & 31;
    let curve = p.low_curve;
    let mut pred = [0i32; 2];
    let mut len = [0u32; 2];

    let finish = |s: i32| -> u16 {
        let mut s = s as u16;
        if curve && s < 256 {
            s = LOW_CURVE[s as usize];
        }
        ((s as u32) << shift) as u16
    };

    for block in out[..full].chunks_exact_mut(8) {
        for l in len.iter_mut() {
            let (used, w) = L_CODE[bits.peek(6) as usize];
            bits.skip(used as u32);
            if w != 0 {
                *l = w as u32;
            }
        }
        for (i, o) in block.iter_mut().enumerate() {
            let par = i & 1;
            let w = len[par];
            let v = if w == 14 {
                bits.get(16) as i32
            } else if w == 0 {
                return Err(Error::corrupt("IIQ L: block without difference width"));
            } else {
                let f = bits.get(w) as i32;
                pred[par].wrapping_add(f + 1 - (1i32 << (w - 1)))
            };
            pred[par] = v;
            *o = finish(v);
        }
    }
    for o in out[full..].iter_mut() {
        *o = finish(bits.get(16) as i32);
    }
    if bits.exhausted() {
        return Err(Error::corrupt("IIQ L: truncated row data"));
    }
    Ok(())
}

/// Codewords of the IIQ S level reset (the bits after the `00` prefix of a level update) and
/// the level each selects: the most frequent levels have the shortest codes.
const S_RESET_CODEWORDS: [(u8, u8, u8); 10] = [
    (2, 0b10, 3),
    (2, 0b11, 2),
    (3, 0b010, 1),
    (3, 0b011, 4),
    (4, 0b0010, 6),
    (4, 0b0011, 5),
    (5, 0b00000, 9),
    (5, 0b00001, 8),
    (5, 0b00010, 0),
    (5, 0b00011, 7),
];

/// Level reset code of IIQ S, indexed by the five bits that follow the `00` prefix:
/// `(bits consumed after the prefix, new level)`.
const S_RESET: [(u8, u8); 32] = expand_prefix_code(&S_RESET_CODEWORDS, 5);

/// Update one channel's level from the stream.
#[inline(always)]
fn s_level<B: BitRead>(bits: &mut B, level: &mut u32) {
    let v = bits.peek(7);
    let t = v >> 5;
    if t != 0 {
        bits.skip(2);
        // Prefix 01, 10, 11 steps the level by -1, 0, +1.
        *level = level.wrapping_add(t).wrapping_sub(2);
    } else {
        let (n, l) = S_RESET[(v & 31) as usize];
        bits.skip(2 + n as u32);
        *level = l as u32;
    }
}

/// Decode one IIQ S row of `out.len()` samples (output already scaled to 16 bits).
pub fn decode_s_row<B: BitRead>(bits: &mut B, out: &mut [u16]) -> Result<()> {
    let full = out.len() & !7;
    let init = bits.get(16) & 7;
    let mut level = [0u32; 2];
    let mut prev = [0i32; 2];

    for block in out[..full].chunks_exact_mut(8) {
        s_level(bits, &mut level[0]);
        s_level(bits, &mut level[1]);
        let c = bits.peek(3);
        let extra = if c >= 4 {
            bits.skip(1);
            0
        } else {
            bits.skip(3);
            c + 1
        };
        let take = init + extra;
        let mut sh = [0u32; 2];
        let mut sub = [0i32; 2];
        for p in 0..2 {
            sh[p] = level[p].wrapping_sub(extra) & 31;
            sub[p] = (0xFFFFu32 >> (((17 - init).wrapping_sub(level[p])) & 31)) as i32;
        }
        for (i, o) in block.iter_mut().enumerate() {
            let p = i & 1;
            let value = if level[p] == 9 {
                bits.get(14) as i32
            } else {
                let coded = if take == 0 { 0 } else { bits.get(take) };
                prev[p].wrapping_add((coded << sh[p]) as i32).wrapping_sub(sub[p])
            };
            prev[p] = value;
            *o = value.wrapping_shl(2).clamp(0, 65535) as u16;
        }
    }
    for o in out[full..].iter_mut() {
        *o = (bits.get(14) << 2) as u16;
    }
    if bits.exhausted() {
        return Err(Error::corrupt("IIQ S: truncated row data"));
    }
    Ok(())
}

/// Undo the key scrambling of the uncompressed IIQ payload formats 1 and 2 on a run of
/// samples (pairs run over the whole raster, independent of row boundaries).
pub fn unscramble_pairs(samples: &mut [u16], akey: u16, bkey: u16, format: u32) {
    let m: u16 = if format == 1 { 0x5555 } else { 0x1354 };
    for pair in samples.chunks_exact_mut(2) {
        let a = pair[0] ^ akey;
        let b = pair[1] ^ bkey;
        pair[0] = (a & m) | (b & !m);
        pair[1] = (b & m) | (a & !m);
    }
}
