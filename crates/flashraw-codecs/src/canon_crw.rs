//! Canon CRW ("CIFF") compressed raw codec, used by the PowerShot G1..G6, Pro1,
//! S30..S70 and EOS D30/D60/10D/300D generation of cameras.
//!
//! ## Bitstream
//!
//! The sensor is coded as one sequential MSB-first bit stream with JPEG-style byte
//! stuffing (`FF 00` → `FF`; an `FF` followed by anything else terminates the stream).
//! Pixels are taken in raster order, eight rows at a time, in blocks of 64 samples.
//! Each block is coded like a JPEG 8x8 block, but in the *sample* domain:
//!
//! - coefficient 0 uses the "first" Huffman table; its leaf is a bit length and the
//!   difference is added to a running value carried across all blocks;
//! - coefficients 1..63 use the "second" table; each leaf is `run << 4 | length`
//!   (skip `run` zero coefficients, then read a `length`-bit difference), leaf 0 ends
//!   the block, leaf `0xFF` is a no-op.
//!
//! A block's 64 differences are then integrated per column parity: every image row
//! restarts two predictors (even/odd columns) at 512, and sample `i` of a block adds
//! its difference to predictor `i & 1`. Blocks run through rows continuously, so a
//! row boundary can fall inside a block.
//!
//! Three table sets exist, selected by the CIFF `0x1835` decoder-table record.
//!
//! ## Low bits
//!
//! The entropy-coded samples are 10 bits. Most later cameras additionally store the
//! two least significant bits of every sample uncompressed (one byte per 4 samples,
//! LSB pair first) *before* the compressed stream, giving 12-bit output. The 2672-wide
//! sensors (G5/S50/S60) add 2 to every resulting value below 512.
//!
//! Decoding is bit-exact, including the handling of
//! out-of-range predictions (samples wrap to 16 bits) and of incomplete Huffman code
//! spaces (an unassigned code consumes no bits and decodes as "end of block").

use flashraw_core::bits::{BitRead, extend_diff};
use flashraw_core::{Error, Result};
use std::sync::OnceLock;

/// Code-length counts (lengths 1..=16) followed by leaves, per table set.
const FIRST_TREE: [[u8; 29]; 3] = [
    [
        0, 1, 4, 2, 3, 1, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x04, 0x03, 0x05, 0x06, 0x02, 0x07, 0x01, 0x08, 0x09, 0x00,
        0x0a, 0x0b, 0xff,
    ],
    [
        0, 2, 2, 3, 1, 1, 1, 1, 2, 0, 0, 0, 0, 0, 0, 0, 0x03, 0x02, 0x04, 0x01, 0x05, 0x00, 0x06, 0x07, 0x09, 0x08,
        0x0a, 0x0b, 0xff,
    ],
    [
        0, 0, 6, 3, 1, 1, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x06, 0x05, 0x07, 0x04, 0x08, 0x03, 0x09, 0x02, 0x00, 0x0a,
        0x01, 0x0b, 0xff,
    ],
];

const SECOND_TREE: [[u8; 180]; 3] = [
    [
        0, 2, 2, 2, 1, 4, 2, 1, 2, 5, 1, 1, 0, 0, 0, 139, 0x03, 0x04, 0x02, 0x05, 0x01, 0x06, 0x07, 0x08, 0x12, 0x13,
        0x11, 0x14, 0x09, 0x15, 0x22, 0x00, 0x21, 0x16, 0x0a, 0xf0, 0x23, 0x17, 0x24, 0x31, 0x32, 0x18, 0x19, 0x33,
        0x25, 0x41, 0x34, 0x42, 0x35, 0x51, 0x36, 0x37, 0x38, 0x29, 0x79, 0x26, 0x1a, 0x39, 0x56, 0x57, 0x28, 0x27,
        0x52, 0x55, 0x58, 0x43, 0x76, 0x59, 0x77, 0x54, 0x61, 0xf9, 0x71, 0x78, 0x75, 0x96, 0x97, 0x49, 0xb7, 0x53,
        0xd7, 0x74, 0xb6, 0x98, 0x47, 0x48, 0x95, 0x69, 0x99, 0x91, 0xfa, 0xb8, 0x68, 0xb5, 0xb9, 0xd6, 0xf7, 0xd8,
        0x67, 0x46, 0x45, 0x94, 0x89, 0xf8, 0x81, 0xd5, 0xf6, 0xb4, 0x88, 0xb1, 0x2a, 0x44, 0x72, 0xd9, 0x87, 0x66,
        0xd4, 0xf5, 0x3a, 0xa7, 0x73, 0xa9, 0xa8, 0x86, 0x62, 0xc7, 0x65, 0xc8, 0xc9, 0xa1, 0xf4, 0xd1, 0xe9, 0x5a,
        0x92, 0x85, 0xa6, 0xe7, 0x93, 0xe8, 0xc1, 0xc6, 0x7a, 0x64, 0xe1, 0x4a, 0x6a, 0xe6, 0xb3, 0xf1, 0xd3, 0xa5,
        0x8a, 0xb2, 0x9a, 0xba, 0x84, 0xa4, 0x63, 0xe5, 0xc5, 0xf3, 0xd2, 0xc4, 0x82, 0xaa, 0xda, 0xe4, 0xf2, 0xca,
        0x83, 0xa3, 0xa2, 0xc3, 0xea, 0xc2, 0xe2, 0xe3, 0xff, 0xff,
    ],
    [
        0, 2, 2, 1, 4, 1, 4, 1, 3, 3, 1, 0, 0, 0, 0, 140, 0x02, 0x03, 0x01, 0x04, 0x05, 0x12, 0x11, 0x06, 0x13, 0x07,
        0x08, 0x14, 0x22, 0x09, 0x21, 0x00, 0x23, 0x15, 0x31, 0x32, 0x0a, 0x16, 0xf0, 0x24, 0x33, 0x41, 0x42, 0x19,
        0x17, 0x25, 0x18, 0x51, 0x34, 0x43, 0x52, 0x29, 0x35, 0x61, 0x39, 0x71, 0x62, 0x36, 0x53, 0x26, 0x38, 0x1a,
        0x37, 0x81, 0x27, 0x91, 0x79, 0x55, 0x45, 0x28, 0x72, 0x59, 0xa1, 0xb1, 0x44, 0x69, 0x54, 0x58, 0xd1, 0xfa,
        0x57, 0xe1, 0xf1, 0xb9, 0x49, 0x47, 0x63, 0x6a, 0xf9, 0x56, 0x46, 0xa8, 0x2a, 0x4a, 0x78, 0x99, 0x3a, 0x75,
        0x74, 0x86, 0x65, 0xc1, 0x76, 0xb6, 0x96, 0xd6, 0x89, 0x85, 0xc9, 0xf5, 0x95, 0xb4, 0xc7, 0xf7, 0x8a, 0x97,
        0xb8, 0x73, 0xb7, 0xd8, 0xd9, 0x87, 0xa7, 0x7a, 0x48, 0x82, 0x84, 0xea, 0xf4, 0xa6, 0xc5, 0x5a, 0x94, 0xa4,
        0xc6, 0x92, 0xc3, 0x68, 0xb5, 0xc8, 0xe4, 0xe5, 0xe6, 0xe9, 0xa2, 0xa3, 0xe3, 0xc2, 0x66, 0x67, 0x93, 0xaa,
        0xd4, 0xd5, 0xe7, 0xf8, 0x88, 0x9a, 0xd7, 0x77, 0xc4, 0x64, 0xe2, 0x98, 0xa5, 0xca, 0xda, 0xe8, 0xf3, 0xf6,
        0xa9, 0xb2, 0xb3, 0xf2, 0xd2, 0x83, 0xba, 0xd3, 0xff, 0xff,
    ],
    [
        0, 0, 6, 2, 1, 3, 3, 2, 5, 1, 2, 2, 8, 10, 0, 117, 0x04, 0x05, 0x03, 0x06, 0x02, 0x07, 0x01, 0x08, 0x09, 0x12,
        0x13, 0x14, 0x11, 0x15, 0x0a, 0x16, 0x17, 0xf0, 0x00, 0x22, 0x21, 0x18, 0x23, 0x19, 0x24, 0x32, 0x31, 0x25,
        0x33, 0x38, 0x37, 0x34, 0x35, 0x36, 0x39, 0x79, 0x57, 0x58, 0x59, 0x28, 0x56, 0x78, 0x27, 0x41, 0x29, 0x77,
        0x26, 0x42, 0x76, 0x99, 0x1a, 0x55, 0x98, 0x97, 0xf9, 0x48, 0x54, 0x96, 0x89, 0x47, 0xb7, 0x49, 0xfa, 0x75,
        0x68, 0xb6, 0x67, 0x69, 0xb9, 0xb8, 0xd8, 0x52, 0xd7, 0x88, 0xb5, 0x74, 0x51, 0x46, 0xd9, 0xf8, 0x3a, 0xd6,
        0x87, 0x45, 0x7a, 0x95, 0xd5, 0xf6, 0x86, 0xb4, 0xa9, 0x94, 0x53, 0x2a, 0xa8, 0x43, 0xf5, 0xf7, 0xd4, 0x66,
        0xa7, 0x5a, 0x44, 0x8a, 0xc9, 0xe8, 0xc8, 0xe7, 0x9a, 0x6a, 0x73, 0x4a, 0x61, 0xc7, 0xf4, 0xc6, 0x65, 0xe9,
        0x72, 0xe6, 0x71, 0x91, 0x93, 0xa6, 0xda, 0x92, 0x85, 0x62, 0xf3, 0xc5, 0xb2, 0xa4, 0x84, 0xba, 0x64, 0xa5,
        0xb3, 0xd2, 0x81, 0xe5, 0xd3, 0xaa, 0xc4, 0xca, 0xf2, 0xb1, 0xe4, 0xd1, 0x83, 0x63, 0xea, 0xc3, 0xe2, 0x82,
        0xf1, 0xa3, 0xc2, 0xa1, 0xc1, 0xe3, 0xa2, 0xe1, 0xff, 0xff,
    ],
];

/// Bytes of uncompressed low-bit data per sample group (4 samples per byte).
pub const LOWBITS_PER_BYTE: usize = 4;

/// Number of bits resolved by the primary lookup table.
const FAST_BITS: u32 = 12;
/// Longest code in any CRW table.
const MAX_BITS: u32 = 16;

// Fast LUT entry (u32):
//   bits 0..5   bits to consume (code length, plus difference length when RESOLVED)
//   bit  5      RESOLVED: bits 16..32 hold the sign-extended difference
//   bit  6      VALID: the code fits in FAST_BITS
//   bits 8..16  leaf
const RESOLVED: u32 = 1 << 5;
const VALID: u32 = 1 << 6;

/// A CRW Huffman table: a `FAST_BITS` lookup with fused difference decoding, plus a
/// complete `MAX_BITS` table (entries `code_len << 8 | leaf`) for long codes.
struct CrwHuff {
    fast: Box<[u32; 1 << FAST_BITS]>,
    /// Indexed by the next 16 bits; entries `len << 8 | leaf`, 0 for unassigned codes.
    full: Vec<u16>,
}

impl CrwHuff {
    /// Build from a `counts[16] ++ leaves` specification.
    fn new(spec: &[u8]) -> Self {
        let (counts, leaves) = spec.split_at(16);
        let mut full = vec![0u16; 1 << MAX_BITS];
        let mut fast = Box::new([0u32; 1 << FAST_BITS]);
        let mut code: u32 = 0;
        let mut k = 0usize;
        for len in 1..=MAX_BITS {
            for _ in 0..counts[len as usize - 1] {
                let leaf = leaves[k];
                k += 1;
                let span = 1u32 << (MAX_BITS - len);
                let start = (code << (MAX_BITS - len)) as usize;
                full[start..start + span as usize].fill(((len as u16) << 8) | leaf as u16);
                if len <= FAST_BITS {
                    let shift = FAST_BITS - len;
                    for fill in 0..(1u32 << shift) {
                        let idx = ((code << shift) | fill) as usize;
                        let dlen = (leaf & 15) as u32;
                        fast[idx] = if leaf == 0xff || dlen == 0 {
                            len | RESOLVED | VALID | ((leaf as u32) << 8)
                        } else if len + dlen <= FAST_BITS {
                            let bits = (idx as u32 >> (FAST_BITS - len - dlen)) & ((1 << dlen) - 1);
                            let diff = extend_diff(bits, dlen);
                            (len + dlen) | RESOLVED | VALID | ((leaf as u32) << 8) | ((diff as i16 as u16 as u32) << 16)
                        } else {
                            len | VALID | ((leaf as u32) << 8)
                        };
                    }
                }
                code += 1;
            }
            code <<= 1;
        }
        CrwHuff { fast, full }
    }

    /// Decode one leaf and its difference. Requires >= 32 buffered bits (one code of
    /// at most 16 bits plus at most 15 difference bits).
    #[inline(always)]
    fn decode<B: BitRead>(&self, bits: &mut B) -> (u32, i32) {
        let e = self.fast[bits.peek_no_fill(FAST_BITS) as usize];
        if e & RESOLVED != 0 {
            bits.skip_no_fill(e & 31);
            return ((e >> 8) & 0xff, (e >> 16) as u16 as i16 as i32);
        }
        let (code_len, leaf) =
            if e & VALID != 0 { (e & 31, (e >> 8) & 0xff) } else { self.slow_entry(bits.peek_no_fill(MAX_BITS)) };
        bits.skip_no_fill(code_len);
        let dlen = leaf & 15;
        if leaf == 0xff || dlen == 0 {
            return (leaf, 0);
        }
        let v = bits.peek_no_fill(dlen);
        bits.skip_no_fill(dlen);
        (leaf, extend_diff(v, dlen))
    }

    #[cold]
    #[inline(never)]
    fn slow_entry(&self, peek: u32) -> (u32, u32) {
        let f = self.full[peek as usize & 0xffff] as u32;
        (f >> 8, f & 0xff)
    }
}

/// MSB-first reader with JPEG-style byte stuffing (`FF 00` → `FF`; `FF` followed by
/// anything else, or the end of data, ends the stream and yields zero bits).
///
/// Unlike the general `BitPumpJpeg`, the rare slow refill passes the state by value so
/// the whole reader stays in registers inside the decode loop.
struct StuffedBits<'a> {
    data: &'a [u8],
    pos: usize,
    cache: u64,
    bits: u32,
    /// Zero bytes fed after the end of the stream.
    overrun: usize,
    stopped: bool,
}

impl<'a> StuffedBits<'a> {
    fn new(data: &'a [u8]) -> Self {
        StuffedBits { data, pos: 0, cache: 0, bits: 0, overrun: 0, stopped: false }
    }
}

/// Byte-wise refill up to more than 56 buffered bits; handles stuffing and the stream end.
#[cold]
#[inline(never)]
fn refill_slow(
    data: &[u8],
    mut pos: usize,
    mut cache: u64,
    mut bits: u32,
    mut overrun: usize,
    mut stopped: bool,
) -> (usize, u64, u32, usize, bool) {
    while bits <= 56 {
        let mut byte = 0u8;
        if !stopped {
            match (data.get(pos), data.get(pos + 1)) {
                (Some(&0xff), Some(&0)) => {
                    byte = 0xff;
                    pos += 2;
                }
                (Some(&0xff), _) | (None, _) => stopped = true,
                (Some(&b), _) => {
                    byte = b;
                    pos += 1;
                }
            }
        }
        if stopped {
            overrun += 1;
        }
        cache |= (byte as u64) << (56 - bits);
        bits += 8;
    }
    (pos, cache, bits, overrun, stopped)
}

impl BitRead for StuffedBits<'_> {
    #[inline(always)]
    fn fill(&mut self) {
        if self.bits >= 32 {
            return;
        }
        self.refill_force();
    }
    #[inline(always)]
    fn refill_force(&mut self) {
        if let Some(chunk) = self.data.get(self.pos..self.pos + 8) {
            let w = u64::from_be_bytes(chunk.try_into().unwrap_or([0xff; 8]));
            // SWAR: any 0xFF byte needs the stuffing-aware path.
            let inv = !w;
            let has_ff = (inv.wrapping_sub(0x0101_0101_0101_0101) & !inv & 0x8080_8080_8080_8080) != 0;
            if !has_ff && !self.stopped {
                self.cache |= w >> self.bits;
                self.pos += ((63 - self.bits) >> 3) as usize;
                self.bits |= 56;
                return;
            }
        }
        (self.pos, self.cache, self.bits, self.overrun, self.stopped) =
            refill_slow(self.data, self.pos, self.cache, self.bits, self.overrun, self.stopped);
    }
    #[inline(always)]
    fn peek_no_fill(&self, n: u32) -> u32 {
        (self.cache >> (64 - n)) as u32
    }
    #[inline(always)]
    fn skip_no_fill(&mut self, n: u32) {
        self.cache <<= n;
        self.bits -= n;
    }
    fn byte_position(&self) -> usize {
        self.pos - (self.bits as usize >> 3)
    }
    fn overrun(&self) -> usize {
        self.overrun
    }
    fn buffered(&self) -> u32 {
        self.bits
    }
}

/// The (first, second) table pair for a decoder-table id; ids above 2 use set 2.
fn tables(table: u32) -> &'static (CrwHuff, CrwHuff) {
    static SETS: [OnceLock<(CrwHuff, CrwHuff)>; 3] = [OnceLock::new(), OnceLock::new(), OnceLock::new()];
    let t = table.min(2) as usize;
    SETS[t].get_or_init(|| (CrwHuff::new(&FIRST_TREE[t]), CrwHuff::new(&SECOND_TREE[t])))
}

/// Parameters of a CRW compressed payload.
#[derive(Debug, Clone, Copy)]
pub struct CrwParams<'a> {
    pub width: usize,
    pub height: usize,
    /// Decoder table id (CIFF record 0x1835); values above 2 select table set 2.
    pub table: u32,
    /// The uncompressed low-bit plane (`width * height / 4` bytes, 4 samples per byte,
    /// LSB pair first), when the file stores one. Missing bytes read as `0xFF`
    /// (all-ones padding past end of file).
    pub lowbits: Option<&'a [u8]>,
    /// G5/S50/S60 quirk: add 2 to 12-bit values below 512.
    pub low_offset_quirk: bool,
}

/// Decode a CRW compressed payload into `out` (`width * height` samples, row-major).
///
/// `stream` starts at the first entropy-coded byte. Samples not covered by whole
/// 64-sample blocks are left as they are in `out` (normally zero). Returns an error if the bit stream ends prematurely.
pub fn decode(stream: &[u8], p: &CrwParams, out: &mut [u16]) -> Result<()> {
    let (w, h) = (p.width, p.height);
    let total = w.checked_mul(h).ok_or_else(|| Error::corrupt("CRW dimensions overflow"))?;
    if w == 0 || h == 0 || out.len() < total {
        return Err(Error::InvalidArgument("CRW: output buffer smaller than width * height".into()));
    }
    let (first, second) = tables(p.table);
    let mut bits = StuffedBits::new(stream);
    let mut carry: i32 = 0;
    let mut base = [512i32; 2];
    // Samples left in the current image row (the predictors restart at each row).
    let mut row_left = 0usize;
    let mut row = 0usize;
    while row < h {
        let stripe_start = row * w;
        let stripe_rows = (h - row).min(8);
        let nblocks = stripe_rows * w / 64;
        for block in 0..nblocks {
            let mut diffs = [0i32; 64];
            // Coefficient 0 (first table): never ends the block.
            bits.fill();
            let (leaf, diff) = first.decode(&mut bits);
            let mut i = 1usize;
            if leaf != 0xff {
                let pos = (leaf >> 4) as usize;
                if pos < 64 {
                    diffs[pos] = diff;
                }
                i += pos;
            }
            while i < 64 {
                bits.fill();
                let (leaf, diff) = second.decode(&mut bits);
                if leaf == 0 {
                    break;
                }
                if leaf != 0xff {
                    i += (leaf >> 4) as usize;
                    if i < 64 {
                        diffs[i] = diff;
                    }
                }
                i += 1;
            }
            if bits.exhausted() {
                return Err(Error::corrupt(format!("truncated CRW bit stream at row {row}")));
            }
            diffs[0] = diffs[0].wrapping_add(carry);
            carry = diffs[0];
            let dst: &mut [u16; 64] = (&mut out[stripe_start + block * 64..stripe_start + block * 64 + 64])
                .try_into()
                .expect("64-sample block");
            if row_left == 0 {
                base = [512, 512];
                row_left = w;
            }
            if row_left >= 64 {
                // Common case: the block lies inside one row.
                row_left -= 64;
                let (mut b0, mut b1) = (base[0], base[1]);
                for (o, d) in dst.chunks_exact_mut(2).zip(diffs.chunks_exact(2)) {
                    b0 = b0.wrapping_add(d[0]);
                    b1 = b1.wrapping_add(d[1]);
                    o[0] = b0 as u16;
                    o[1] = b1 as u16;
                }
                base = [b0, b1];
            } else {
                for (k, (o, &d)) in dst.iter_mut().zip(diffs.iter()).enumerate() {
                    if row_left == 0 {
                        base = [512, 512];
                        row_left = w;
                    }
                    row_left -= 1;
                    let b = &mut base[k & 1];
                    *b = b.wrapping_add(d);
                    *o = *b as u16;
                }
            }
        }
        if let Some(low) = p.lowbits {
            merge_lowbits(out, total, stripe_start, w, low, p.low_offset_quirk);
        }
        row += 8;
    }
    Ok(())
}

/// Combine the 10-bit samples of the eight-row stripe at `stripe_start` with their low
/// bits. The low-bit plane maps linearly onto sample indices (byte `n / 4` holds sample
/// `n` at bit `2 * (n % 4)`).
fn merge_lowbits(out: &mut [u16], total: usize, stripe_start: usize, w: usize, low: &[u8], quirk: bool) {
    let end = (stripe_start + 8 * w).min(total);
    let dst = &mut out[stripe_start..end];
    let src = low.get(stripe_start / 4..).unwrap_or(&[]);
    for (n, chunk) in dst.chunks_mut(4).enumerate() {
        let byte = src.get(n).copied().unwrap_or(0xff) as u32;
        for (k, v) in chunk.iter_mut().enumerate() {
            let mut val = ((*v as u32) << 2) | ((byte >> (2 * k)) & 3);
            if quirk && val < 512 {
                val += 2;
            }
            *v = val as u16;
        }
    }
}

/// Whether the payload carries a separate low-bit plane, using a heuristic over
/// the first 16 KiB of the file: scanning from offset 540, an `FF` byte followed by a
/// non-zero byte cannot occur in the byte-stuffed compressed stream, so it indicates
/// the (unstuffed) low-bit plane; `FF 00` pairs indicate a stream starting right there.
/// Without any `FF` the plane is assumed present.
pub fn has_lowbits(file: &[u8]) -> bool {
    let end = file.len().min(0x4000);
    let mut ret = true;
    let mut i = 540;
    while i + 1 < end {
        if file[i] == 0xff {
            if file[i + 1] != 0 {
                return true;
            }
            ret = false;
        }
        i += 1;
    }
    ret
}

#[cfg(test)]
mod tests {
    use super::*;
    use flashraw_core::bits::BitPumpMsb;

    /// Canonical codes `(leaf, code, len)` of a table spec.
    fn codes(spec: &[u8]) -> Vec<(u8, u32, u32)> {
        let (counts, leaves) = spec.split_at(16);
        let mut out = vec![];
        let (mut code, mut k) = (0u32, 0usize);
        for len in 1..=16u32 {
            for _ in 0..counts[len as usize - 1] {
                out.push((leaves[k], code, len));
                code += 1;
                k += 1;
            }
            code <<= 1;
        }
        out
    }

    struct Writer {
        out: Vec<u8>,
        acc: u32,
        n: u32,
    }
    impl Writer {
        fn new() -> Self {
            Writer { out: vec![], acc: 0, n: 0 }
        }
        fn put(&mut self, v: u32, len: u32) {
            for i in (0..len).rev() {
                self.acc = (self.acc << 1) | ((v >> i) & 1);
                self.n += 1;
                if self.n == 8 {
                    let b = self.acc as u8;
                    self.out.push(b);
                    if b == 0xff {
                        self.out.push(0); // byte stuffing
                    }
                    self.acc = 0;
                    self.n = 0;
                }
            }
        }
        fn finish(mut self) -> Vec<u8> {
            while self.n != 0 {
                self.put(0, 1);
            }
            self.out.extend_from_slice(&[0; 16]);
            self.out
        }
    }

    fn bit_len(d: i32) -> u32 {
        if d == 0 { 0 } else { 32 - d.unsigned_abs().leading_zeros() }
    }
    fn diff_bits(d: i32, len: u32) -> u32 {
        if d < 0 { (d - 1) as u32 & ((1 << len) - 1) } else { d as u32 }
    }

    /// A minimal CRW encoder (no low bits): returns the stream for `img`.
    fn encode(img: &[u16], w: usize, h: usize, table: usize) -> Vec<u8> {
        let first = codes(&FIRST_TREE[table]);
        let second = codes(&SECOND_TREE[table]);
        let find = |t: &[(u8, u32, u32)], leaf: u8| *t.iter().find(|c| c.0 == leaf).expect("leaf in table");
        let mut wr = Writer::new();
        let mut carry = 0i32;
        let mut base = [512i32; 2];
        let mut row_left = 0usize;
        let mut row = 0;
        while row < h {
            let nblocks = (h - row).min(8) * w / 64;
            for block in 0..nblocks {
                let mut diffs = [0i32; 64];
                for (i, d) in diffs.iter_mut().enumerate() {
                    if row_left == 0 {
                        base = [512, 512];
                        row_left = w;
                    }
                    row_left -= 1;
                    let v = img[row * w + block * 64 + i] as i32;
                    *d = v - base[i & 1];
                    base[i & 1] = v;
                }
                let dc = diffs[0] - carry;
                carry = diffs[0];
                let len = bit_len(dc);
                let (_, c, l) = find(&first, len as u8);
                wr.put(c, l);
                wr.put(diff_bits(dc, len), len);
                let last = (1..64).rev().find(|&i| diffs[i] != 0).unwrap_or(0);
                let mut run = 0;
                for &d in &diffs[1..=last] {
                    if d == 0 {
                        run += 1;
                        continue;
                    }
                    while run > 15 {
                        let (_, c, l) = find(&second, 0xf0);
                        wr.put(c, l);
                        run -= 16;
                    }
                    let len = bit_len(d);
                    let (_, c, l) = find(&second, ((run << 4) | len) as u8);
                    wr.put(c, l);
                    wr.put(diff_bits(d, len), len);
                    run = 0;
                }
                if last < 63 {
                    let (_, c, l) = find(&second, 0);
                    wr.put(c, l);
                }
            }
            row += 8;
        }
        wr.finish()
    }

    fn pseudo_image(w: usize, h: usize, seed: u32) -> Vec<u16> {
        let mut x = seed;
        (0..w * h)
            .map(|i| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                // Smooth-ish content with occasional large jumps and flat runs.
                let base = 300 + ((i % w) as u32 * 3 + (i / w) as u32) % 400;
                let noise = if x % 7 == 0 { x % 300 } else { x % 6 };
                if (i / 37) % 5 == 0 { 512 } else { (base + noise).min(1023) as u16 as u32 }
            })
            .map(|v| v as u16)
            .collect()
    }

    #[test]
    fn tables_are_canonical_and_mostly_complete() {
        for t in 0..3 {
            let f = CrwHuff::new(&FIRST_TREE[t]);
            // First tables are complete: every 16-bit pattern decodes.
            assert!(f.full.iter().all(|&e| e != 0));
            let s = CrwHuff::new(&SECOND_TREE[t]);
            // Second tables leave a few all-ones 16-bit codes unassigned (0-length leaf 0).
            let unassigned = s.full.iter().filter(|&&e| e == 0).count();
            assert!(unassigned > 0 && unassigned <= 32, "table {t}: {unassigned}");
            assert_eq!(s.full[0xffff], 0);
        }
    }

    #[test]
    fn lut_matches_full_table() {
        // Every 16-bit prefix must decode identically through the fast and the slow path.
        for t in 0..3 {
            for spec in [&FIRST_TREE[t][..], &SECOND_TREE[t][..]] {
                let huff = CrwHuff::new(spec);
                for prefix in (0u32..1 << 16).step_by(7) {
                    let data = [(prefix >> 8) as u8, prefix as u8, 0xa5, 0x5a, 0x33, 0xcc, 0, 0, 0, 0, 0, 0];
                    let mut p = BitPumpMsb::new(&data);
                    p.fill();
                    let (leaf, diff) = huff.decode(&mut p);
                    // Reference: full table + explicit difference read.
                    let f = huff.full[prefix as usize] as u32;
                    let mut q = BitPumpMsb::new(&data);
                    q.skip(f >> 8);
                    let rleaf = f & 0xff;
                    let dlen = rleaf & 15;
                    let rdiff = if rleaf == 0xff || dlen == 0 { 0 } else { extend_diff(q.get(dlen), dlen) };
                    assert_eq!((leaf, diff), (rleaf, rdiff), "table {t} prefix {prefix:04x}");
                    assert_eq!(p.byte_position(), q.byte_position());
                }
            }
        }
    }

    #[test]
    fn roundtrip_all_tables() {
        // Width not a multiple of 64 (row boundaries inside blocks) and a partial last stripe.
        for (w, h) in [(136usize, 20usize), (64, 8), (200, 13)] {
            for table in 0..3 {
                let img = pseudo_image(w, h, 7 + table as u32);
                let stream = encode(&img, w, h, table);
                let mut out = vec![0u16; w * h];
                let p = CrwParams { width: w, height: h, table: table as u32, lowbits: None, low_offset_quirk: false };
                decode(&stream, &p, &mut out).unwrap();
                let covered = |i: usize| {
                    let row = i / w / 8 * 8;
                    let nblocks = (h - row).min(8) * w / 64;
                    i - row * w < nblocks * 64
                };
                for i in 0..w * h {
                    let expect = if covered(i) { img[i] } else { 0 };
                    assert_eq!(out[i], expect, "{w}x{h} table {table} sample {i}");
                }
            }
        }
    }

    #[test]
    fn lowbits_merge_and_quirk() {
        let (w, h) = (128usize, 8usize);
        let img = pseudo_image(w, h, 3);
        let stream = encode(&img, w, h, 1);
        let low: Vec<u8> = (0..w * h / 4).map(|i| (i * 37) as u8).collect();
        let mut out = vec![0u16; w * h];
        let p = CrwParams { width: w, height: h, table: 1, lowbits: Some(&low), low_offset_quirk: true };
        decode(&stream, &p, &mut out).unwrap();
        for i in 0..w * h {
            let mut v = ((img[i] as u32) << 2) | ((low[i / 4] as u32 >> (2 * (i % 4))) & 3);
            if v < 512 {
                v += 2;
            }
            assert_eq!(out[i] as u32, v);
        }
    }

    #[test]
    fn truncated_stream_is_an_error() {
        let (w, h) = (128usize, 16usize);
        let img = pseudo_image(w, h, 5);
        let stream = encode(&img, w, h, 0);
        let mut out = vec![0u16; w * h];
        let p = CrwParams { width: w, height: h, table: 0, lowbits: None, low_offset_quirk: false };
        assert!(decode(&stream[..stream.len() / 3], &p, &mut out).is_err());
        assert!(decode(&[], &p, &mut out).is_err());
        // Garbage never panics.
        for seed in 0..20u32 {
            let junk: Vec<u8> =
                (0..300u32).map(|i| (i.wrapping_mul(2654435761).wrapping_add(seed) >> 13) as u8).collect();
            let _ = decode(&junk, &p, &mut out);
        }
        assert!(decode(&stream, &p, &mut out[..10]).is_err());
    }

    #[test]
    fn lowbits_heuristic() {
        let mut f = vec![0u8; 2000];
        assert!(has_lowbits(&f)); // no FF at all
        f[700] = 0xff;
        assert!(!has_lowbits(&f)); // FF 00: stuffed stream
        f[900] = 0xff;
        f[901] = 0x12;
        assert!(has_lowbits(&f)); // FF xx: raw low-bit plane
        f[100] = 0xff;
        f[101] = 0x55; // before offset 540: ignored
        f[901] = 0;
        assert!(!has_lowbits(&f));
    }
}
