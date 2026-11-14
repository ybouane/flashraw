//! Sony ARW 1.x (DSLR-A100 .. A390) Huffman-coded column stream.
//!
//! The payload is one MSB-first bit stream of 12-bit samples stored as differences. Columns are
//! coded from the rightmost to column 0; inside a column the even rows come first, then the odd
//! rows. One running sum covers the whole stream. Every difference is a length symbol from a
//! fixed 18-word prefix code followed by that many magnitude bits (JPEG-style extension).
//!
//! Decoding works on *tiles* of [`TILE`] adjacent columns: the differences of a tile are summed
//! straight into a small row-major buffer (so the column-major stream is transposed while it
//! is decoded) and the buffer's rows are then copied to the image. Large streams are first
//! entropy-decoded by speculation on all threads; the tiles are then rebuilt in parallel from
//! the recovered differences and per-tile starting sums.

use crate::par_entropy::{self, EndCheck, SymbolStream};
use flashraw_core::{Error, Result, par};

/// Columns per tile (64 bytes of a tile row).
const TILE: usize = 32;
/// Bits looked at to find the length symbol.
const CODE_BITS: u32 = 15;

/// `(codeword length, difference bit count)` in code-assignment order: entry `i` owns the 15-bit
/// patterns following those of entry `i - 1`.
const CODE: [(u32, u32); 18] = [
    (15, 17),
    (15, 16),
    (14, 15),
    (13, 14),
    (12, 13),
    (11, 12),
    (10, 11),
    (9, 10),
    (8, 9),
    (7, 8),
    (6, 7),
    (5, 6),
    (4, 5),
    (3, 4),
    (3, 3),
    (3, 0),
    (2, 2),
    (2, 1),
];

/// Index of the codeword that starts a 15-bit pattern.
const fn entry_of(pattern: u32) -> usize {
    let mut start = 0u32;
    let mut i = 0;
    while i < CODE.len() {
        let end = start + (1 << (CODE_BITS - CODE[i].0));
        if pattern < end {
            return i;
        }
        start = end;
        i += 1;
    }
    CODE.len() - 1
}

/// Lookup by the next 8 bits: bits 0..6 total bits of the difference, 6..10 the codeword length
/// `c`, 10..15 the magnitude bit count `n`; 0 when the codeword is longer than 8 bits.
const fn build_short() -> [u16; 256] {
    let mut t = [0u16; 256];
    let mut p = 0;
    while p < 256 {
        let (c, n) = CODE[entry_of((p as u32) << (CODE_BITS - 8))];
        if c <= 8 {
            t[p] = (c + n) as u16 | (c as u16) << 6 | (n as u16) << 10;
        }
        p += 1;
    }
    t
}

static SHORT: [u16; 256] = build_short();

/// One difference from the MSB-aligned window `w` (at least 32 valid bits): the difference and
/// the number of bits it occupies.
#[inline(always)]
fn next_diff(w: u64) -> (u16, u32) {
    let e = u32::from(SHORT[(w >> 56) as usize]);
    if e == 0 {
        return long_diff(w);
    }
    let (clen, n) = ((e >> 6) & 15, e >> 10);
    // The n magnitude bits follow the codeword; the top one set means a positive difference.
    let x = (((w << clen) >> 1) >> (63 - n)) as u32;
    let negative = ((x << 1) >> n) & 1 ^ 1;
    (x.wrapping_sub(negative * ((1 << n) - 1)) as u16, e & 63)
}

/// [`next_diff`] for codewords longer than 8 bits: these are a run of zeros and a one, so the
/// length follows from the leading zeros.
#[inline(never)]
fn long_diff(w: u64) -> (u16, u32) {
    match w.leading_zeros() {
        lz @ 8..=13 => {
            let (clen, n) = (lz + 1, lz + 2);
            let x = (((w << clen) >> 1) >> (63 - n)) as u32;
            let negative = ((x << 1) >> n) & 1 ^ 1;
            (x.wrapping_sub(negative * ((1 << n) - 1)) as u16, clen + n)
        }
        14 => (0x8000, 15),
        _ => {
            let x = (w << 15) >> 47;
            let negative = ((x << 1) >> 17) & 1 ^ 1;
            (x.wrapping_sub(negative * ((1 << 17) - 1)) as u16, 32)
        }
    }
}

/// Read 8 bytes big-endian at `pos`; bytes past the end read as zero.
#[inline(always)]
fn load8(src: &[u8], pos: usize) -> u64 {
    if let Some(b) = src.get(pos..).and_then(|s| s.first_chunk::<8>()) {
        u64::from_be_bytes(*b)
    } else {
        let mut b = [0u8; 8];
        if let Some(rest) = src.get(pos..) {
            b[..rest.len()].copy_from_slice(rest);
        }
        u64::from_be_bytes(b)
    }
}

/// Bit reader with a 64-bit accumulator refilled to at least 56 bits per symbol.
struct Bits<'a> {
    src: &'a [u8],
    next: usize,
    acc: u64,
    nbits: u32,
}

impl<'a> Bits<'a> {
    fn new(src: &'a [u8]) -> Self {
        Bits { src, next: 0, acc: 0, nbits: 0 }
    }

    #[inline(always)]
    fn refill(&mut self) {
        self.acc |= load8(self.src, self.next) >> self.nbits;
        self.next += ((63 - self.nbits) >> 3) as usize;
        self.nbits |= 56;
    }

    #[inline(always)]
    fn consume(&mut self, bits: u32) {
        self.acc <<= bits;
        self.nbits -= bits;
    }

    /// Decode `out.len()` differences.
    fn diffs_into(&mut self, out: &mut [u16]) {
        for o in out {
            self.refill();
            let (d, used) = next_diff(self.acc);
            *o = d;
            self.consume(used);
        }
    }

    /// Bits consumed so far (padding included).
    fn consumed(&self) -> u64 {
        self.next as u64 * 8 - u64::from(self.nbits)
    }
}

/// Shape of one decode.
#[derive(Clone, Copy)]
struct Shape {
    width: usize,
    /// Rows present in the stream per column are `coded`; of them `keep` are stored.
    coded: usize,
    keep: usize,
}

impl Shape {
    /// Samples per column in the stream.
    fn per_column(&self) -> usize {
        self.coded.div_ceil(2) + if self.has_odd() { self.coded / 2 } else { 0 }
    }
    fn has_odd(&self) -> bool {
        self.coded % 2 == 0
    }
}

/// Turn a column of differences into running sums in place, continuing from `sum`.
#[inline(always)]
fn accumulate(col: &mut [u16], mut sum: u16) -> u16 {
    for x in col {
        sum = sum.wrapping_add(*x);
        *x = sum;
    }
    sum
}

/// Distance between the columns of a tile buffer: a column plus padding, so that the columns do not
/// map onto the same cache sets when they are read side by side.
fn column_stride(s: Shape) -> usize {
    s.per_column() + 32
}

/// Write image row `r` of a tile (`dst`, `ncols` samples) from the summed columns in `cols`
/// (stream order: the first column is the rightmost). Odd rows of an odd-height stream are zero.
#[inline(always)]
fn tile_row(cols: &[u16], stride: usize, s: Shape, r: usize, dst: &mut [u16]) {
    let rank = if r % 2 == 0 {
        r / 2
    } else if s.has_odd() {
        s.coded.div_ceil(2) + r / 2
    } else {
        dst.fill(0);
        return;
    };
    for (o, col) in dst.iter_mut().rev().zip(cols.chunks_exact(stride)) {
        *o = col[rank];
    }
}

/// Number of columns in tile `k` (the last one may be narrow).
fn tile_cols(width: usize, k: usize) -> usize {
    TILE.min(width - k * TILE)
}

fn decode_sequential(src: &[u8], out: &mut [u16], s: Shape) -> Result<()> {
    let mut bits = Bits::new(src);
    let stride = column_stride(s);
    let per_col = s.per_column();
    let mut cols = vec![0u16; stride * TILE];
    let tiles = s.width.div_ceil(TILE);
    let mut sum = 0u16;
    for k in (0..tiles).rev() {
        let ncols = tile_cols(s.width, k);
        for col in cols.chunks_exact_mut(stride).take(ncols) {
            let col = &mut col[..per_col];
            bits.diffs_into(col);
            sum = accumulate(col, sum);
        }
        for (r, row) in out.chunks_exact_mut(s.width).take(s.keep).enumerate() {
            tile_row(&cols, stride, s, r, &mut row[k * TILE..][..ncols]);
        }
    }
    if bits.consumed() > src.len() as u64 * 8 {
        return Err(truncated());
    }
    Ok(())
}

fn truncated() -> Error {
    Error::corrupt("Sony ARW1 stream is truncated")
}

/// Speculative-decoding view of the stream: one difference per symbol.
struct DiffStream<'a> {
    src: &'a [u8],
}

impl SymbolStream for DiffStream<'_> {
    type Pump<'p>
        = u64
    where
        Self: 'p;
    const WIDTH: usize = 1;
    fn cycle(&self) -> usize {
        1
    }
    fn period(&self) -> usize {
        1
    }
    fn total_bits(&self) -> u64 {
        self.src.len() as u64 * 8
    }
    fn end_check(&self, pos: u64) -> EndCheck {
        if pos <= self.total_bits() { EndCheck::Valid } else { EndCheck::Invalid }
    }
    fn pump_at(&self, bit: u64) -> u64 {
        bit
    }
    fn position(p: &u64) -> u64 {
        *p
    }
    #[inline(always)]
    fn decode(&self, p: &mut u64, _phase: usize, out: &mut [u16]) -> bool {
        let (d, used) = next_diff(load8(self.src, (*p >> 3) as usize) << (*p & 7));
        out[0] = d;
        *p += u64::from(used);
        true
    }
}

/// Parallel route: differences from speculation, tiles rebuilt on all threads. `None` when
/// speculation could not decide (the caller decodes sequentially).
fn decode_speculative(src: &[u8], out: &mut [u16], s: Shape) -> Option<()> {
    let per_col = s.per_column();
    let symbols = s.width * per_col;
    let chunk = par_entropy::default_chunk(src.len())?;
    let vals = par_entropy::decode(&DiffStream { src }, symbols, chunk)?;
    let tiles = s.width.div_ceil(TILE);
    let grid = par::GridWriter::new(
        &mut out[..s.width * s.keep],
        u32::try_from(s.width).ok()?,
        u32::try_from(s.keep).ok()?,
        1,
        (0, 0),
        TILE as u32,
        u32::try_from(s.keep).ok()?,
        u32::try_from(s.width).ok()?,
        u32::try_from(s.keep).ok()?,
    )?;
    // Stream position of tile k's first difference: the tile holding the rightmost columns comes first.
    let first_of = |k: usize| (s.width - k * TILE - tile_cols(s.width, k)) * per_col;
    let totals = par::map((0..tiles).collect(), true, |k| {
        let n = tile_cols(s.width, k) * per_col;
        let mut buf = vec![0u16; n];
        vals.copy_to(first_of(k), &mut buf);
        buf.iter().fold(0u16, |a, &d| a.wrapping_add(d))
    });
    let mut starts = vec![0u16; tiles];
    let mut sum = 0u16;
    for k in (0..tiles).rev() {
        starts[k] = sum;
        sum = sum.wrapping_add(totals[k]);
    }
    let stride = column_stride(s);
    par::for_each_index(tiles, true, |k| {
        let ncols = tile_cols(s.width, k);
        let mut cols = vec![0u16; stride * ncols];
        let mut sum = starts[k];
        for (j, col) in cols.chunks_exact_mut(stride).enumerate() {
            let col = &mut col[..per_col];
            vals.copy_to(first_of(k) + j * per_col, col);
            sum = accumulate(col, sum);
        }
        let Some(mut dst) = grid.claim(k as u32, 0) else { return };
        for r in 0..s.keep {
            tile_row(&cols, stride, s, r, dst.row(r));
        }
    });
    Some(())
}

/// Decode an ARW1 stream into `out` (`width * stored_rows` samples, row-major, one `u16` per pixel).
///
/// `coded_rows` rows are present in the stream per column; only rows below `stored_rows` are
/// written. Errors with a corrupt-file error (no structured reason) when the stream is
/// truncated, and with `Error::InvalidArgument` when `out` holds fewer than `width * stored_rows` samples.
pub fn decode(src: &[u8], out: &mut [u16], width: usize, coded_rows: usize, stored_rows: usize) -> Result<()> {
    decode_par(src, out, width, coded_rows, stored_rows, false)
}

/// [`decode`], using several threads for large streams when `parallel` is set. Output and
/// errors are identical to [`decode`] whatever the thread count.
pub fn decode_par(
    src: &[u8],
    out: &mut [u16],
    width: usize,
    coded_rows: usize,
    stored_rows: usize,
    parallel: bool,
) -> Result<()> {
    let needed =
        width.checked_mul(stored_rows).ok_or_else(|| Error::InvalidArgument("ARW1 image size overflows".into()))?;
    if out.len() < needed {
        return Err(Error::InvalidArgument("ARW1 output buffer is too small".into()));
    }
    let shape = Shape { width, coded: coded_rows, keep: stored_rows.min(coded_rows) };
    if width == 0 || shape.keep == 0 {
        return Ok(());
    }
    // Every difference takes at least two bits: a stream shorter than that is truncated, and
    // this bounds the work for absurd geometry.
    let symbols = width.checked_mul(shape.per_column()).ok_or_else(truncated)?;
    if (symbols as u128) * 2 > src.len() as u128 * 8 {
        return Err(truncated());
    }
    if parallel && symbols >= crate::ljpeg_par::PARALLEL_MIN_SAMPLES && decode_speculative(src, out, shape).is_some() {
        return Ok(());
    }
    decode_sequential(src, out, shape)
}

#[cfg(test)]
mod unit {
    use super::*;

    /// Reference: one difference at bit `pos` of `src`, straight from the format description.
    fn reference(src: &[u8], pos: &mut usize) -> u16 {
        let bit = |i: usize| u32::from(src.get(i / 8).map_or(0, |b| b >> (7 - i % 8) & 1));
        let take = |pos: &mut usize, n: u32| {
            let mut v = 0u32;
            for _ in 0..n {
                v = v << 1 | bit(*pos);
                *pos += 1;
            }
            v
        };
        let mut look = 0u32;
        for i in 0..15 {
            look = look << 1 | bit(*pos + i);
        }
        let (clen, n) = CODE[entry_of(look)];
        *pos += clen as usize;
        match n {
            0 => 0,
            16 => 0x8000,
            _ => {
                let x = take(pos, n);
                if x >= 1 << (n - 1) { x as u16 } else { x.wrapping_sub((1 << n) - 1) as u16 }
            }
        }
    }

    #[test]
    fn lookups_match_reference() {
        let mut x = 0x9e37_79b9_7f4a_7c15u64;
        for len in [1usize, 2, 3, 40, 1000] {
            let src: Vec<u8> = (0..len * 8)
                .map(|_| {
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    // Bias to short codes: mostly set bits give long magnitudes, few give short.
                    (x >> 11) as u8 & (x >> 23) as u8
                })
                .collect();
            for n in [1usize, 2, 3, 7, 50] {
                let mut bits = Bits::new(&src);
                let mut got = vec![0u16; n];
                bits.diffs_into(&mut got);
                let mut pos = 0;
                let want: Vec<u16> = (0..n).map(|_| reference(&src, &mut pos)).collect();
                assert_eq!(got, want, "len {len} n {n}");
                assert_eq!(bits.consumed(), pos as u64);
            }
        }
    }
}
