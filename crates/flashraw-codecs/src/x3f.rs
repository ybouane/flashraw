//! Sigma / Foveon X3F entropy decoders.
//!
//! Three families of raw image sections plus the CAMF calibration section:
//!
//! * the layered TRUE coding: three independent planes, each a sequence of signed
//!   differences coded with a small explicit prefix code, predicted on a 2 x 2 phase grid;
//! * the 10-bit Huffman coding: every row an independent bit stream of prefix-coded
//!   steps of a running sum per colour, with a global offset applied afterwards, and its
//!   uncompressed "simple" variant (three packed table indices per 32-bit word);
//! * the CAMF section, scrambled with a byte cipher (type 2) or coded with the TRUE
//!   difference code (types 4 and 5).
//!
//! All bit streams are read most significant bit first. Reading past the end of the data
//! yields zero bits; every decoder reports `CorruptFile` when it consumed bits that do
//! not exist, and never loops longer than the amount of data justifies.

use flashraw_core::bits::{BitPumpMsb, BitRead};
use flashraw_core::{Error, Result};
use std::sync::Mutex;
use std::sync::atomic::{AtomicI32, Ordering};

// ------------------------------------------------------------------------------------------
// Explicit prefix codes
// ------------------------------------------------------------------------------------------

/// Number of leading bits resolved by the direct lookup table of a [`PrefixCode`].
const DIRECT_BITS: u32 = 12;
const DIRECT_SIZE: usize = 1 << DIRECT_BITS;

/// Entry of the direct table: the low 5 bits are the code length (0: not resolved here),
/// the upper 27 bits the symbol, or, for length 0, the range of longer code words that start
/// with this prefix (first index in 14 bits, count in 13 bits).
#[derive(Debug, Clone, Copy, Default)]
struct Direct(u32);

const SYMBOL_LIMIT: u32 = 1 << 27;
const LONG_LIMIT: usize = 1 << 14;
const SLOT_LIMIT: u32 = 1 << 13;

impl Direct {
    #[inline(always)]
    fn len(self) -> u32 {
        self.0 & 31
    }
    #[inline(always)]
    fn value(self) -> u32 {
        self.0 >> 5
    }
}

#[derive(Debug, Clone, Copy)]
struct Long {
    /// Code word left-aligned in 32 bits.
    word: u32,
    len: u8,
    value: u32,
}

/// A binary prefix code given as explicit `(code, length, value)` triples (not canonical,
/// possibly incomplete). Code words up to 32 bits are supported.
#[derive(Debug, Clone)]
pub struct PrefixCode {
    /// Indexed by the next `direct_bits` bits.
    direct: Box<[Direct; DIRECT_SIZE]>,
    /// Code words longer than `direct_bits`, sorted by their left-aligned word.
    long: Vec<Long>,
}

#[cold]
fn bad_symbol() -> Error {
    Error::corrupt("X3F: bit pattern matches no code word")
}

impl PrefixCode {
    /// Build from `(code, length, value)` triples; `code` is right-aligned, MSB first (only
    /// its low `length` bits count). Fails for an empty list, a length outside 1..=32, or
    /// code words that are not prefix-free.
    pub fn new(codes: &[(u32, u32, u32)]) -> Result<Self> {
        if codes.is_empty() {
            return Err(Error::corrupt("X3F: empty prefix code"));
        }
        let mut words: Vec<Long> = Vec::with_capacity(codes.len());
        for &(bits, len, value) in codes {
            if len == 0 || len > 32 {
                return Err(Error::corrupt("X3F: prefix code length out of range"));
            }
            let right = if len == 32 { bits } else { bits & ((1u32 << len) - 1) };
            if value >= SYMBOL_LIMIT {
                return Err(Error::corrupt("X3F: prefix code value too large"));
            }
            words.push(Long { word: ((right as u64) << (32 - len)) as u32, len: len as u8, value });
        }
        words.sort_unstable_by_key(|w| (w.word, w.len));
        for pair in words.windows(2) {
            let end = pair[0].word as u64 + (1u64 << (32 - pair[0].len as u32));
            if (pair[1].word as u64) < end {
                return Err(Error::corrupt("X3F: prefix code is not prefix-free"));
            }
        }
        let direct_bits = DIRECT_BITS;
        let mut direct: Box<[Direct; DIRECT_SIZE]> = vec![Direct::default(); DIRECT_SIZE]
            .into_boxed_slice()
            .try_into()
            .map_err(|_| Error::corrupt("X3F: prefix code table"))?;
        let mut long = Vec::new();
        for w in &words {
            if w.len as u32 <= direct_bits {
                let first = (w.word >> (32 - direct_bits)) as usize;
                let count = 1usize << (direct_bits - w.len as u32);
                direct[first..first + count].fill(Direct(w.value << 5 | w.len as u32));
            } else {
                let slot = &mut direct[(w.word >> (32 - direct_bits)) as usize];
                let (first, count) = if slot.0 == 0 {
                    (long.len() as u32, 0)
                } else {
                    (slot.value() >> 13, slot.value() & (SLOT_LIMIT - 1))
                };
                if long.len() >= LONG_LIMIT || count + 1 >= SLOT_LIMIT {
                    return Err(Error::corrupt("X3F: too many long code words"));
                }
                *slot = Direct(((first << 13) | (count + 1)) << 5);
                long.push(*w);
            }
        }
        Ok(Self { direct, long })
    }

    /// One symbol, or `None` for a bit pattern that matches no code word.
    #[inline(always)]
    fn lookup<B: BitRead>(&self, bits: &mut B) -> Option<u32> {
        bits.fill_to(DIRECT_BITS);
        let e = self.direct[bits.peek_no_fill(DIRECT_BITS) as usize & (DIRECT_SIZE - 1)];
        if e.len() != 0 {
            bits.skip_no_fill(e.len());
            return Some(e.value());
        }
        bits.fill();
        // The cold part only looks at a copy of the next 32 bits, so hot loops can keep
        // the bit reader in registers.
        let (len, value) = self.resolve_long(bits.peek_no_fill(32), e)?;
        bits.skip_no_fill(len);
        Some(value)
    }

    /// Code word (length, value) at the start of the 32 bits `x`, among the code words
    /// longer than the direct table.
    #[cold]
    #[inline(never)]
    fn resolve_long(&self, x: u32, slot: Direct) -> Option<(u32, u32)> {
        let (first, count) = ((slot.value() >> 13) as usize, (slot.value() & (SLOT_LIMIT - 1)) as usize);
        let candidates = self.long.get(first..)?.get(..count)?;
        let i = candidates.partition_point(|w| w.word <= x).checked_sub(1)?;
        let w = candidates[i];
        if (x ^ w.word) >> (32 - w.len as u32) != 0 {
            return None;
        }
        Some((w.len as u32, w.value))
    }

    /// Decode one symbol from `bits`.
    #[inline(always)]
    pub fn decode<B: BitRead>(&self, bits: &mut B) -> Result<u32> {
        self.lookup(bits).ok_or_else(bad_symbol)
    }
}

// ------------------------------------------------------------------------------------------
// The signed difference of the TRUE coding
// ------------------------------------------------------------------------------------------

/// Largest symbol (difference bit length) of the TRUE coding.
const MAX_DIFF_BITS: u32 = 31;
/// Width of the combined (code + difference) lookup of [`TrueTable`].
const COMBINED_BITS: u32 = 12;
/// Returned by the unchecked difference reader on invalid data (no real difference equals it).
const BAD_DIFF: i32 = i32::MIN;

/// Value of the `len`-bit pattern `v` as a signed difference (top bit set: itself; else
/// `v - (2^len - 1)`).
#[inline(always)]
fn signed_diff(v: u32, len: u32) -> i32 {
    if len == 0 {
        0
    } else if v >> (len - 1) != 0 {
        v as i32
    } else {
        (v as i64 - ((1i64 << len) - 1)) as i32
    }
}

/// Decoding table of the TRUE coding.
#[derive(Debug, Clone)]
pub struct TrueTable {
    /// Indexed by the next `COMBINED_BITS` bits: `diff << 16 | total length` when the code
    /// word and the whole difference fit, else 0.
    combined: Vec<u32>,
    code: PrefixCode,
}

impl TrueTable {
    /// Wrap a prefix code whose symbols are difference bit lengths.
    pub fn new(code: PrefixCode) -> Self {
        let mut combined = vec![0u32; 1usize << COMBINED_BITS];
        // Recover the code words from the direct table and the long list.
        let mut words: Vec<Long> = code.long.clone();
        let mut i = 0usize;
        while i < code.direct.len() {
            let e = code.direct[i];
            if e.len() != 0 {
                words.push(Long {
                    word: ((i as u64) << (32 - DIRECT_BITS)) as u32,
                    len: e.len() as u8,
                    value: e.value(),
                });
                i += 1usize << (DIRECT_BITS - e.len());
            } else {
                i += 1;
            }
        }
        for w in words {
            let total = w.len as u32 + w.value;
            if w.value > MAX_DIFF_BITS || total > COMBINED_BITS {
                continue;
            }
            let prefix = (w.word >> (32 - w.len as u32)) as usize;
            for v in 0..1u32 << w.value {
                let d = signed_diff(v, w.value);
                let entry = ((d as i16 as u16 as u32) << 16) | total;
                let first = ((prefix << w.value) | v as usize) << (COMBINED_BITS - total);
                combined[first..first + (1usize << (COMBINED_BITS - total))].fill(entry);
            }
        }
        Self { combined, code }
    }

    /// One difference, or `BAD_DIFF` for invalid data.
    #[inline(always)]
    fn next(&self, bits: &mut BitPumpMsb) -> i32 {
        bits.fill();
        let e = self.combined[bits.peek_no_fill(COMBINED_BITS) as usize];
        if e & 0xffff != 0 {
            bits.skip_no_fill(e & 0xffff);
            return (e as i32) >> 16;
        }
        let mut tmp = *bits;
        let d = self.next_slow(&mut tmp);
        *bits = tmp;
        d
    }

    #[cold]
    #[inline(never)]
    fn next_slow(&self, bits: &mut BitPumpMsb) -> i32 {
        match self.code.lookup(bits) {
            Some(l) if l <= MAX_DIFF_BITS => {
                if l == 0 {
                    0
                } else {
                    let v = bits.get(l);
                    signed_diff(v, l)
                }
            }
            _ => BAD_DIFF,
        }
    }

    /// Decode one signed difference.
    #[inline(always)]
    pub fn diff(&self, bits: &mut BitPumpMsb) -> Result<i32> {
        match self.next(bits) {
            BAD_DIFF => Err(bad_symbol()),
            d => Ok(d),
        }
    }

    /// Decode one row of the 2 x 2 phase prediction. `out` holds the stored columns,
    /// `cols >= out.len()` is the coded width; `start` is the row-start state of this
    /// row parity. Returns false on an invalid code.
    #[inline(always)]
    fn row(&self, bits: &mut BitPumpMsb, start: &mut [u16; 2], out: &mut [u16], cols: usize) -> bool {
        let mut b = *bits;
        let n = out.len();
        let mut a = [0u16; 2];
        for x in 0..cols.min(2) {
            let d = self.next(&mut b);
            if d == BAD_DIFF {
                return false;
            }
            start[x] = start[x].wrapping_add(d as u16);
            a[x] = start[x];
            if x < n {
                out[x] = a[x];
            }
        }
        if n > 2 {
            let mut pairs = out[2..].chunks_exact_mut(2);
            let (mut a0, mut a1) = (a[0], a[1]);
            for pair in &mut pairs {
                let d0 = self.next(&mut b);
                a0 = a0.wrapping_add(d0 as u16);
                let d1 = self.next(&mut b);
                a1 = a1.wrapping_add(d1 as u16);
                if d0 == BAD_DIFF || d1 == BAD_DIFF {
                    return false;
                }
                pair[0] = a0;
                pair[1] = a1;
            }
            if let [last] = pairs.into_remainder() {
                let d = self.next(&mut b);
                if d == BAD_DIFF {
                    return false;
                }
                *last = a0.wrapping_add(d as u16);
            }
        }
        for _ in n.max(2)..cols {
            if self.next(&mut b) == BAD_DIFF {
                return false;
            }
        }
        *bits = b;
        true
    }

    /// Reference version of [`TrueTable::row`]: one checked sample at a time, stopping
    /// before the first sample that needs bits beyond the data. Returns the number of
    /// samples stored in `out` (`out.len()` = columns requested) or `None` on an invalid
    /// code.
    fn row_checked(&self, bits: &mut BitPumpMsb, start: &mut [u16; 2], out: &mut [u16]) -> Option<usize> {
        let mut a = [0u16; 2];
        for (x, o) in out.iter_mut().enumerate() {
            let before = *bits;
            let d = self.next(bits);
            if d == BAD_DIFF {
                return None;
            }
            if bits.past_end() > 0 {
                *bits = before;
                return Some(x);
            }
            let p = x & 1;
            a[p] = if x < 2 {
                start[p] = start[p].wrapping_add(d as u16);
                start[p]
            } else {
                a[p].wrapping_add(d as u16)
            };
            *o = a[p];
        }
        Some(out.len())
    }
}

// ------------------------------------------------------------------------------------------
// TRUE header and planes
// ------------------------------------------------------------------------------------------

/// Parsed header of a TRUE-coded raw image section (the bytes after the 28-byte `SECi` header).
#[derive(Debug, Clone)]
pub struct TrueHeader {
    /// Quattro plane sizes `(columns, rows)` (bottom, middle, top) when present.
    pub planes: Option<[(u32, u32); 3]>,
    /// Start value of each plane's prediction.
    pub seeds: [u16; 3],
    /// `(code_size, code)` pairs, terminator included; the symbol of entry `i` is `i`.
    pub table: Vec<(u8, u8)>,
    /// Byte size of each plane's coded data.
    pub plane_sizes: [u32; 3],
    /// Offset of the first plane's data within the parsed slice.
    pub data_offset: usize,
}

fn le16(d: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([d[at], d[at + 1]])
}

fn le32(d: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([d[at], d[at + 1], d[at + 2], d[at + 3]])
}

/// Prefix code of a `(len, code byte)` table: entry `i` with `len > 0` is the word made of
/// the top `len` bits of its byte and has the symbol `i`; the first zero length ends the list.
fn table_code(table: &[(u8, u8)]) -> Result<PrefixCode> {
    let mut codes = Vec::new();
    for (i, &(len, byte)) in table.iter().enumerate() {
        if len == 0 {
            break;
        }
        if len > 8 {
            return Err(Error::corrupt("X3F: code length above 8 in table"));
        }
        codes.push(((byte >> (8 - len)) as u32, len as u32, i as u32));
    }
    PrefixCode::new(&codes)
}

impl TrueHeader {
    /// Parse the header.
    pub fn parse(d: &[u8], quattro: bool) -> Result<Self> {
        let short = || Error::corrupt("X3F: TRUE header is truncated");
        let mut at = 0usize;
        let planes = if quattro {
            if d.len() < 12 {
                return Err(short());
            }
            let mut p = [(0u32, 0u32); 3];
            for (i, q) in p.iter_mut().enumerate() {
                *q = (le16(d, i * 4) as u32, le16(d, i * 4 + 2) as u32);
            }
            at = 12;
            Some(p)
        } else {
            None
        };
        if d.len() < at + 8 {
            return Err(short());
        }
        let seeds = [le16(d, at), le16(d, at + 2), le16(d, at + 4)];
        at += 8; // seeds and one ignored u16
        let mut table = Vec::new();
        loop {
            let Some(e) = d.get(at..at + 2) else { return Err(short()) };
            table.push((e[0], e[1]));
            at += 2;
            if e[0] == 0 {
                break;
            }
        }
        if quattro {
            at += 4;
        }
        if d.len() < at + 12 {
            return Err(short());
        }
        let plane_sizes = [le32(d, at), le32(d, at + 4), le32(d, at + 8)];
        Ok(Self { planes, seeds, table, plane_sizes, data_offset: at + 12 })
    }

    /// The decoding table for the planes.
    pub fn table(&self) -> Result<TrueTable> {
        Ok(TrueTable::new(table_code(&self.table)?))
    }

    /// The prefix code (symbol = bit length of the difference).
    pub fn code(&self) -> Result<PrefixCode> {
        table_code(&self.table)
    }

    /// Byte ranges `(start, end)` of each plane within the data that follows the header.
    /// Planes are padded to 16 bytes; each may be read up to the end of all the data.
    pub fn plane_ranges(&self, data_len: usize) -> Result<[(usize, usize); 3]> {
        let mut start = 0u64;
        let mut ranges = [(0usize, data_len); 3];
        for (r, &size) in ranges.iter_mut().zip(&self.plane_sizes) {
            r.0 = start as usize;
            start += (size as u64).div_ceil(16) * 16;
        }
        if start > data_len as u64 {
            return Err(Error::corrupt("X3F: plane data exceeds the section"));
        }
        Ok(ranges)
    }
}

/// Decode one TRUE plane of `cols x rows` samples into `out` (`out_cols` samples per row,
/// `out_cols <= cols`; the extra columns on the right are decoded and discarded).
pub fn decode_true_plane(
    code: &TrueTable,
    data: &[u8],
    seed: u16,
    cols: usize,
    rows: usize,
    out_cols: usize,
    out: &mut [u16],
) -> Result<()> {
    if out_cols > cols {
        return Err(Error::corrupt("X3F: plane output wider than the plane"));
    }
    let stored = out_cols.checked_mul(rows).filter(|&n| n <= out.len());
    let Some(stored) = stored else {
        return Err(Error::corrupt("X3F: plane output buffer too small"));
    };
    if rows == 0 || out_cols == 0 {
        return Ok(());
    }
    // Every sample takes at least one bit.
    if (cols as u128) * (rows as u128) > (data.len() as u128) * 8 {
        return Err(Error::corrupt("X3F: plane data is truncated"));
    }
    let mut bits = BitPumpMsb::new(data);
    let mut start = [[seed; 2]; 2];
    for (r, row) in out[..stored].chunks_exact_mut(out_cols).enumerate() {
        if !code.row(&mut bits, &mut start[r & 1], row, cols) {
            return Err(bad_symbol());
        }
        if bits.past_end() > 0 {
            return Err(Error::corrupt("X3F: plane data is truncated"));
        }
    }
    Ok(())
}

// ------------------------------------------------------------------------------------------
// 10-bit Huffman coding
// ------------------------------------------------------------------------------------------

/// Code of the 10-bit Huffman format from its 1024-entry table and mapping table.
pub fn huffman_code(table: &[u32], mapping: &[u16]) -> Result<PrefixCode> {
    let mut codes = Vec::new();
    for (i, &t) in table.iter().enumerate() {
        if t == 0 {
            continue;
        }
        let Some(&sym) = mapping.get(i) else {
            return Err(Error::corrupt("X3F: Huffman table larger than the mapping"));
        };
        codes.push((t & 0x07ff_ffff, t >> 27, sym as u32));
    }
    PrefixCode::new(&codes)
}

/// Rows per parallel task of the row-wise decoders.
const ROWS_PER_TASK: usize = 8;

#[derive(Default)]
struct Failure(Mutex<Option<Error>>);

impl Failure {
    fn set(&self, e: Error) {
        if let Ok(mut g) = self.0.lock() {
            g.get_or_insert(e);
        }
    }
    fn into_result(self) -> Result<()> {
        match self.0.into_inner() {
            Ok(Some(e)) => Err(e),
            _ => Ok(()),
        }
    }
}

/// Decode a row-indexed 10-bit Huffman image (3 interleaved colours) into `out`
/// (`cols * rows * 3` samples).
pub fn decode_huffman(
    code: &PrefixCode,
    data: &[u8],
    row_offsets: &[u32],
    cols: usize,
    rows: usize,
    out: &mut [u16],
    parallel: bool,
) -> Result<()> {
    let row_len = cols.checked_mul(3).ok_or_else(|| Error::corrupt("X3F: image too wide"))?;
    if cols == 0 || row_offsets.len() < rows {
        return Err(Error::corrupt("X3F: Huffman row table is missing"));
    }
    let total = row_len.checked_mul(rows).filter(|&n| n <= out.len());
    let Some(total) = total else {
        return Err(Error::corrupt("X3F: Huffman output buffer too small"));
    };
    if rows == 0 {
        return Ok(());
    }
    let offsets = &row_offsets[..rows];
    // Cheap validation up front: every row must start inside the data and have room for
    // at least one bit per symbol.
    for &o in offsets {
        let o = o as usize;
        if o >= data.len() || (data.len() - o) as u128 * 8 < row_len as u128 {
            return Err(Error::corrupt("X3F: Huffman row offset outside the data"));
        }
    }
    let out = &mut out[..total];
    let lowest = AtomicI32::new(0);
    let failure = Failure::default();
    flashraw_core::par::for_each_row_chunk(out, row_len, ROWS_PER_TASK, parallel, |first, chunk| {
        let mut low = 0i16;
        for (i, row) in chunk.chunks_exact_mut(row_len).enumerate() {
            let mut bits = BitPumpMsb::new(&data[offsets[first + i] as usize..]);
            let mut sum = [0i16; 3];
            for px in row.chunks_exact_mut(3) {
                for (s, o) in sum.iter_mut().zip(px.iter_mut()) {
                    let Some(step) = code.lookup(&mut bits) else {
                        failure.set(bad_symbol());
                        return;
                    };
                    *s = s.wrapping_add(step as u16 as i16);
                    low = low.min(*s);
                    *o = *s as u16;
                }
            }
            if bits.past_end() > 0 {
                failure.set(Error::corrupt("X3F: Huffman row is truncated"));
                return;
            }
        }
        lowest.fetch_min(low as i32, Ordering::Relaxed);
    });
    failure.into_result()?;
    // Shift so that the lowest sum becomes 0; 32768 deliberately wraps to -32768.
    let shift = (-lowest.load(Ordering::Relaxed)) as i16;
    if shift != 0 {
        flashraw_core::par::for_each_row_chunk(out, row_len, 64, parallel, |_, chunk| {
            for v in chunk {
                *v = (*v as i16).wrapping_add(shift).max(0) as u16;
            }
        });
    }
    Ok(())
}

/// Decode the uncompressed variant (three packed indices per 32-bit word).
#[allow(clippy::too_many_arguments)]
pub fn decode_simple(
    data: &[u8],
    row_stride: usize,
    mapping: &[u16],
    bits: u32,
    cols: usize,
    rows: usize,
    out: &mut [u16],
    parallel: bool,
) -> Result<()> {
    if !(8..=12).contains(&bits) {
        return Err(Error::corrupt("X3F: unsupported index width"));
    }
    let row_len = cols.checked_mul(3).filter(|&n| cols != 0 && rows != 0 && n <= usize::MAX / 4);
    let Some(row_len) = row_len else {
        return Err(Error::corrupt("X3F: bad image width"));
    };
    if row_stride < cols * 4 {
        return Err(Error::corrupt("X3F: row stride smaller than a row"));
    }
    let total = row_len.checked_mul(rows).filter(|&n| n <= out.len());
    let Some(total) = total else {
        return Err(Error::corrupt("X3F: output buffer too small"));
    };
    let need = (rows - 1).checked_mul(row_stride).and_then(|n| n.checked_add(cols * 4));
    if need.is_none_or(|n| n > data.len()) {
        return Err(Error::corrupt("X3F: uncompressed image data is truncated"));
    }
    // Step per index; indices past the mapping step by 0, no mapping means identity.
    let size = 1usize << bits;
    let steps: Vec<u16> =
        (0..size).map(|i| if mapping.is_empty() { i as u16 } else { mapping.get(i).copied().unwrap_or(0) }).collect();
    let mask = (size - 1) as u32;
    let out = &mut out[..total];
    flashraw_core::par::for_each_row_chunk(out, row_len, ROWS_PER_TASK, parallel, |first, chunk| {
        for (i, row) in chunk.chunks_exact_mut(row_len).enumerate() {
            let src = &data[(first + i) * row_stride..][..cols * 4];
            let mut sum = [0u16; 3];
            for (px, w) in row.chunks_exact_mut(3).zip(src.chunks_exact(4)) {
                let word = u32::from_le_bytes([w[0], w[1], w[2], w[3]]);
                for (k, (s, o)) in sum.iter_mut().zip(px.iter_mut()).enumerate() {
                    *s = s.wrapping_add(steps[((word >> (k as u32 * bits)) & mask) as usize]);
                    *o = if (*s as i16) > 0 { *s } else { 0 };
                }
            }
        }
    });
    Ok(())
}

// ------------------------------------------------------------------------------------------
// CAMF
// ------------------------------------------------------------------------------------------

/// Decoder state of a coded CAMF block (types 4 and 5).
enum Coded {
    /// Type 4: 12-bit samples on the TRUE prediction, packed two per three bytes.
    Packed {
        cols: usize,
        /// Samples not yet decoded.
        remaining: u64,
        start: [[u16; 2]; 2],
        row_index: usize,
        /// First sample of an unfinished pair.
        pending: Option<u16>,
    },
    /// Type 5: one running byte per difference.
    Running { sum: u8 },
}

enum Camf<'a> {
    /// Everything is decoded.
    Done,
    Coded {
        table: TrueTable,
        bits: BitPumpMsb<'a>,
        state: Coded,
    },
    /// A failed stream: what was decoded stays available.
    Failed,
}

/// Incremental decoder of an X3F `SECc` (CAMF) section.
pub struct CamfStream<'a> {
    out: Vec<u8>,
    size: usize,
    mode: Camf<'a>,
}

/// Bytes of a type-5 block decoded between two end-of-data checks.
const RUN_BLOCK: usize = 4096;

impl<'a> CamfStream<'a> {
    /// Parse the section header (the whole section, starting at its `SECc` signature).
    pub fn new(section: &'a [u8]) -> Result<Self> {
        if section.len() < 28 {
            return Err(Error::corrupt("X3F: CAMF section is shorter than its header"));
        }
        let kind = le32(section, 8);
        let w = [le32(section, 12), le32(section, 16), le32(section, 20), le32(section, 24)];
        let data = &section[28..];
        match kind {
            2 => {
                let mut key = w[3];
                let out = data
                    .iter()
                    .map(|&b| {
                        key = key.wrapping_mul(1597).wrapping_add(51749) % 244_944;
                        b ^ cipher_mask(key)
                    })
                    .collect::<Vec<u8>>();
                let size = out.len();
                Ok(Self { out, size, mode: Camf::Done })
            }
            4 | 5 => {
                if data.len() <= 32 {
                    return Err(Error::corrupt("X3F: CAMF block has no coded data"));
                }
                let mut entries = Vec::new();
                for e in data[..32].chunks_exact(2) {
                    entries.push((e[0], e[1]));
                    if e[0] == 0 {
                        break;
                    }
                }
                if entries.last().is_some_and(|e| e.0 != 0) {
                    return Err(Error::corrupt("X3F: CAMF code table is not terminated"));
                }
                let table = TrueTable::new(table_code(&entries)?);
                let size = w[0] as usize;
                if size as u64 > 12 * (data.len() - 32) as u64 {
                    return Err(Error::corrupt("X3F: CAMF block larger than its data can code"));
                }
                let seed = w[1];
                let mut stream = Self { out: Vec::new(), size, mode: Camf::Done };
                if size == 0 {
                    return Ok(stream);
                }
                let state = if kind == 5 {
                    Coded::Running { sum: seed as u8 }
                } else {
                    let samples = w[2] as u64 * w[3] as u64;
                    if samples == 0 {
                        stream.out = vec![0; size];
                        return Ok(stream);
                    }
                    Coded::Packed {
                        cols: w[2] as usize,
                        remaining: samples,
                        start: [[seed as u16; 2]; 2],
                        row_index: 0,
                        pending: None,
                    }
                };
                stream.mode = Camf::Coded { table, bits: BitPumpMsb::new(&data[32..]), state };
                Ok(stream)
            }
            _ => Err(Error::corrupt("X3F: unknown CAMF coding")),
        }
    }

    /// The bytes decoded so far.
    pub fn decoded(&self) -> &[u8] {
        &self.out
    }

    /// True when the whole block is decoded.
    pub fn is_done(&self) -> bool {
        matches!(self.mode, Camf::Done)
    }

    /// Decode until at least `len` bytes (or the whole block) are available.
    pub fn fill(&mut self, len: usize) -> Result<()> {
        let want = len.min(self.size);
        loop {
            let Camf::Coded { table, bits, state } = &mut self.mode else {
                return match self.mode {
                    Camf::Failed => Err(Error::corrupt("X3F: CAMF data is truncated")),
                    _ => Ok(()),
                };
            };
            if self.out.len() >= want {
                return Ok(());
            }
            let step = match state {
                Coded::Running { sum } => {
                    let n = (self.size - self.out.len()).min(RUN_BLOCK);
                    run_block(table, bits, sum, &mut self.out, n)
                }
                Coded::Packed { cols, remaining, start, row_index, pending } => {
                    // Enough samples to complete the block (a pair yields 3 bytes).
                    let need = ((self.size - self.out.len()) as u64 * 2).div_ceil(3) + 2;
                    let n = (*cols as u64).min(*remaining).min(need) as usize;
                    packed_row(table, bits, &mut start[*row_index & 1], n, pending, &mut self.out).map(|stored| {
                        *remaining -= stored as u64;
                        *row_index += 1;
                        stored == n
                    })
                }
            };
            if step != Some(true) {
                self.mode = Camf::Failed;
                self.out.truncate(self.size);
                return Err(match step {
                    None => bad_symbol(),
                    _ => Error::corrupt("X3F: CAMF data is truncated"),
                });
            }
            let finished = match &mut self.mode {
                Camf::Coded { state: Coded::Packed { remaining, pending, .. }, .. } => {
                    let all = *remaining == 0 || self.out.len() >= self.size;
                    if all {
                        if let Some(a) = pending.take() {
                            self.out.push(((a & 15) << 4) as u8);
                        }
                    }
                    all
                }
                _ => self.out.len() >= self.size,
            };
            if finished {
                self.out.resize(self.size, 0);
                self.mode = Camf::Done;
            }
        }
    }
}

/// Decode `n` bytes of a type-5 block. `Some(true)` when all were decoded; `Some(false)`
/// when the data ended first (the bytes from existing bits stay in `out`); `None` for an
/// invalid code.
fn run_block(table: &TrueTable, bits: &mut BitPumpMsb, sum: &mut u8, out: &mut Vec<u8>, n: usize) -> Option<bool> {
    let (saved_bits, saved_sum, saved_len) = (*bits, *sum, out.len());
    let mut s = *sum;
    out.reserve(n);
    for _ in 0..n {
        let d = table.next(bits);
        if d == BAD_DIFF {
            return None;
        }
        s = s.wrapping_add(d as u8);
        out.push(s);
    }
    if bits.past_end() == 0 {
        *sum = s;
        return Some(true);
    }
    // The block ran into the end of the data: redo it, stopping at the first missing bit.
    *bits = saved_bits;
    *sum = saved_sum;
    out.truncate(saved_len);
    for _ in 0..n {
        let before = *bits;
        let d = table.next(bits);
        if d == BAD_DIFF {
            return None;
        }
        if bits.past_end() > 0 {
            *bits = before;
            return Some(false);
        }
        *sum = sum.wrapping_add(d as u8);
        out.push(*sum);
    }
    Some(true)
}

/// Append the 12-bit sample `v` to a packed byte stream.
#[inline(always)]
fn pack_sample(v: u16, pending: &mut Option<u16>, out: &mut Vec<u8>) {
    let v = v & 0xfff;
    match pending.take() {
        None => {
            out.push((v >> 4) as u8);
            *pending = Some(v);
        }
        Some(a) => {
            out.push((((a & 15) << 4) | (v >> 8)) as u8);
            out.push(v as u8);
        }
    }
}

/// Decode one row of `n` samples of a type-4 block into packed bytes. Returns the number
/// of samples stored (less than `n` when the data ended), or `None` for an invalid code.
fn packed_row(
    table: &TrueTable,
    bits: &mut BitPumpMsb,
    start: &mut [u16; 2],
    n: usize,
    pending: &mut Option<u16>,
    out: &mut Vec<u8>,
) -> Option<usize> {
    let mut row = vec![0u16; n];
    let saved = (*bits, *start);
    if !table.row(bits, start, &mut row, n) {
        return None;
    }
    let stored = if bits.past_end() == 0 {
        n
    } else {
        (*bits, *start) = saved;
        table.row_checked(bits, start, &mut row)?
    };
    for &v in &row[..stored] {
        pack_sample(v, pending, out);
    }
    Some(stored)
}

/// Key-stream byte of the type-2 cipher for key `k` (`k < 244944`).
#[inline(always)]
fn cipher_mask(k: u32) -> u8 {
    let k = k as u64;
    let t = (k * 301_593_171) >> 24;
    ((((256 * k - t) >> 1) + t) >> 17) as u8
}

/// Decode a whole CAMF section.
pub fn decode_camf(section: &[u8]) -> Result<Vec<u8>> {
    let mut s = CamfStream::new(section)?;
    s.fill(usize::MAX)?;
    Ok(s.out)
}
