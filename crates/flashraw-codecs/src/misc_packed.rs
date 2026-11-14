//! Unpackers for headerless sensor dumps: generic bit-packed rows with load flags,
//! MIPI RAW10, Android loose RAW10 and plain 16-bit words.
//!
//! Layout summary:
//!
//! * Generic rows read a stream of little-endian words (8, 16, 24 or 32 bits), each consumed
//!   most significant bit first. Layouts whose rows start on a word boundary are decoded
//!   row by row (independent rows); the rest (pad bytes, interlace, 24-bit words, rows that
//!   end inside a word) run through one sequential bit reader.
//! * MIPI RAW10: four samples in five bytes (four high bytes, then a byte of 2-bit remainders).
//! * Loose RAW10: six 10-bit samples, least significant first, in each little-endian 64-bit word.
//! * 16-bit rows: plain words with an optional right shift and optional bottom-up order.

use flashraw_core::{Error, PackOrder, Result, par};

use crate::packed::unpack_row;

/// Geometry of a generic bit-packed payload.
#[derive(Debug, Clone, Copy)]
pub struct PackedParams {
    /// Samples per row.
    pub width: usize,
    /// Rows.
    pub height: usize,
    /// Bits per sample.
    pub bits: u32,
    /// Layout flags (see the spec).
    pub flags: u32,
}

/// Rows handled per parallel task.
const ROWS_PER_TASK: usize = 32;

const FLAG_PAD_BYTE: u32 = 1;
const FLAG_INTERLACE: u32 = 2;
const FLAG_SECOND_FIELD_MID_FILE: u32 = 4;
const FLAG_WORD_MASK: u32 = 24;
const FLAG_SWAP_PAIRS: u32 = 64;
const FLAG_EVEN_ROWS: u32 = 128;

impl PackedParams {
    /// Stored bytes per row.
    pub fn row_bytes(&self) -> usize {
        let b = self.width * self.bits as usize / 8;
        if self.flags & FLAG_EVEN_ROWS != 0 { b + (b & 1) } else { b }
    }

    fn word_bits(&self) -> u32 {
        8 + (self.flags & FLAG_WORD_MASK)
    }

    /// Whether every row can be decoded on its own.
    fn rows_independent(&self) -> bool {
        self.flags & (FLAG_PAD_BYTE | FLAG_INTERLACE) == 0
            && self.word_bits() != 24
            && (self.row_bytes() * 8) % self.word_bits() as usize == 0
    }
}

/// Decode a generic bit-packed payload starting at `offset` of `file` into `out`.
pub fn packed_rows(file: &[u8], offset: usize, p: PackedParams, out: &mut [u16], parallel: bool) -> Result<()> {
    let (w, h) = (p.width, p.height);
    if p.bits == 0 || p.bits > 16 || w == 0 || out.len() < w * h {
        return Err(Error::corrupt("invalid packed raster geometry"));
    }
    if h == 0 {
        return Ok(());
    }
    if !p.rows_independent() {
        packed_rows_reference(file, offset, p, out);
        return Ok(());
    }
    let src = file.get(offset..).unwrap_or(&[]);
    let bwide = p.row_bytes();
    let wb = p.word_bits();
    let swap = p.flags & FLAG_SWAP_PAIRS != 0;
    par::for_each_row_chunk(&mut out[..w * h], w, ROWS_PER_TASK, parallel, |first, chunk| {
        let mut scratch: Vec<u8> = Vec::new();
        for (i, row) in chunk.chunks_exact_mut(w).enumerate() {
            let start = (first + i) * bwide;
            if start > src.len() {
                break;
            }
            let window = &src[start..(start + bwide).min(src.len())];
            decode_word_row(window, row, p.bits, wb, bwide, &mut scratch);
            if swap {
                for pair in row.chunks_exact_mut(2) {
                    pair.swap(0, 1);
                }
            }
        }
    });
    Ok(())
}

/// Decode one row of `row.len()` samples of `bits` bits from `window`, with everything
/// past the window reading as zero. `full` is the nominal row length in bytes.
fn decode_word_row(window: &[u8], row: &mut [u16], bits: u32, word_bits: u32, full: usize, scratch: &mut Vec<u8>) {
    let need_bits = row.len() * bits as usize;
    if window.len() == full && need_bits <= full * 8 {
        unpack_words(window, row, bits, word_bits);
        return;
    }
    // Short or overrunning row: decode from a zero-extended copy of the window.
    let need_bytes = need_bits.div_ceil(8);
    scratch.clear();
    scratch.extend_from_slice(window);
    scratch.resize(window.len().max(need_bytes).next_multiple_of(4) + 4, 0);
    // Bytes beyond the window must read as zero even when the window is longer than needed.
    unpack_words(scratch, row, bits, word_bits);
}

/// Unpack `dst.len()` samples of `bits` bits from `src`, read as little-endian words of
/// `word_bits` bits consumed most significant bit first. Missing bytes read as zero.
fn unpack_words(src: &[u8], dst: &mut [u16], bits: u32, word_bits: u32) {
    match (word_bits, bits) {
        (8, _) => {
            // Plain big-endian bit packing; only whole bytes the row needs may be read, which
            // the shared row kernels guarantee.
            let need = (dst.len() * bits as usize).div_ceil(8);
            if src.len() >= need {
                unpack_row(&src[..need], dst, bits, PackOrder::MsbBytes, true);
            } else {
                let mut padded = src.to_vec();
                padded.resize(need, 0);
                unpack_row(&padded, dst, bits, PackOrder::MsbBytes, true);
            }
        }
        (16, 16) => {
            let n = dst.len().min(src.len() / 2);
            unpack_row(&src[..n * 2], &mut dst[..n], 16, PackOrder::LsbBytes, true);
            dst[n..].fill(0);
        }
        // 16-bit little-endian words read from the top are the `Msb16` order, which has a
        // vector kernel at 12 bits.
        (16, 12) => unpack_row(src, dst, 12, PackOrder::Msb16, true),
        (16, 10) => words16_10(src, dst),
        (16, _) => words_generic::<true>(src, dst, bits),
        _ => words_generic::<false>(src, dst, bits),
    }
}

/// The 32-bit chunk of the stream at byte `i` for 16-bit (`H16`) or 32-bit words; bytes past
/// the end are zero.
#[inline(always)]
fn stream_chunk<const H16: bool>(src: &[u8], i: usize) -> u32 {
    let v = match src.get(i..i + 4) {
        Some(b) => u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
        None => {
            let mut b = [0u8; 4];
            if let Some(s) = src.get(i..) {
                b[..s.len()].copy_from_slice(s);
            }
            u32::from_le_bytes(b)
        }
    };
    if H16 { v.rotate_left(16) } else { v }
}

fn words_generic<const H16: bool>(src: &[u8], dst: &mut [u16], bits: u32) {
    let mask = (1u64 << bits) - 1;
    let (mut acc, mut n, mut pos) = (0u64, 0u32, 0usize);
    for d in dst {
        if n < bits {
            acc = (acc << 32) | stream_chunk::<H16>(src, pos) as u64;
            pos += 4;
            n += 32;
        }
        n -= bits;
        *d = ((acc >> n) & mask) as u16;
    }
}

/// 10-bit samples in 16-bit words: five words carry eight samples.
fn words16_10(src: &[u8], dst: &mut [u16]) {
    let mut at = 0usize;
    let mut done = 0usize;
    for o in dst.chunks_exact_mut(8) {
        let Some(b) = src.get(at..at + 10) else { break };
        let v = (u64::from(u16::from_le_bytes([b[0], b[1]])) << 48)
            | (u64::from(u16::from_le_bytes([b[2], b[3]])) << 32)
            | (u64::from(u16::from_le_bytes([b[4], b[5]])) << 16)
            | u64::from(u16::from_le_bytes([b[6], b[7]]));
        let t = u16::from_le_bytes([b[8], b[9]]);
        for (k, d) in o[..6].iter_mut().enumerate() {
            *d = ((v >> (54 - 10 * k)) & 0x3ff) as u16;
        }
        o[6] = (((v & 0xf) as u16) << 6) | (t >> 10);
        o[7] = t & 0x3ff;
        at += 10;
        done += 8;
    }
    words_generic_from::<true>(src, at, &mut dst[done..], 10);
}

fn words_generic_from<const H16: bool>(src: &[u8], at: usize, dst: &mut [u16], bits: u32) {
    words_generic::<H16>(src.get(at..).unwrap_or(&[]), dst, bits);
}

/// Samples between two stray bytes in the padded layouts.
const PAD_GROUP: usize = 10;

/// Where the second field of an interlaced file begins when the layout says it starts in
/// the middle of the file: a quarter of the file length, rounded down to a multiple of four
/// bytes (that is, half the length, with the length first rounded down to a multiple of 8).
fn second_field_start(file_len: usize) -> usize {
    file_len / 8 * 4
}

/// Where a row begins inside a run of word-packed rows.
///
/// Rows follow each other on a grid of `word_bits`-bit words starting at the segment's
/// first byte; row `j` starts `j * row_bytes * 8` bits into the grid.
struct RowStart {
    /// First byte of the word holding the row's first bit.
    byte: usize,
    /// Bits of that word that precede the row.
    bit: usize,
    /// Next unread byte once the previous row's samples have been consumed.
    resume_byte: usize,
}

fn row_start(origin: usize, j: usize, p: &PackedParams) -> RowStart {
    let word_bits = p.word_bits() as usize;
    let word_bytes = word_bits / 8;
    let first_bit = j * p.row_bytes() * 8;
    let words_before_row =
        if j == 0 { 0 } else { ((j - 1) * p.row_bytes() * 8 + p.width * p.bits as usize).div_ceil(word_bits) };
    RowStart {
        byte: origin + first_bit / word_bits * word_bytes,
        bit: first_bit % word_bits,
        resume_byte: origin + words_before_row * word_bytes,
    }
}

/// Cursor over little-endian words: the next unread byte, plus a window of the most recent
/// bits of which the `avail` lowest are still unread (negative: that many bits are skipped).
struct WordCursor {
    pos: usize,
    window: u64,
    avail: i64,
}

impl WordCursor {
    fn at(pos: usize) -> Self {
        WordCursor { pos, window: 0, avail: 0 }
    }

    /// Read the next `bits`-bit sample, most significant bit of the word first. Bytes beyond
    /// the file read as zero.
    #[inline(always)]
    fn sample(&mut self, file: &[u8], bits: u32, word_bits: usize, mask: u64) -> u16 {
        self.avail -= i64::from(bits);
        while self.avail < 0 {
            self.window <<= word_bits;
            for i in 0..word_bits / 8 {
                if let Some(&b) = file.get(self.pos + i) {
                    self.window |= u64::from(b) << (8 * i);
                }
            }
            self.pos += word_bits / 8;
            self.avail += word_bits as i64;
        }
        ((self.window >> self.avail) & mask) as u16
    }
}

/// Sequential decode of any packed layout; defines the exact output of [`packed_rows`].
///
/// Stored row `i` lands in image row `r`; every row is addressed by an explicit byte and bit
/// offset. Without padding bytes that offset is a closed form of the row's index; with them
/// the stray bytes shift the word grid, so the cursor is carried from row to row. Decoding
/// stops silently at the first row whose resume byte lies beyond the file.
pub fn packed_rows_reference(file: &[u8], offset: usize, p: PackedParams, out: &mut [u16]) {
    let (w, h) = (p.width, p.height);
    let word_bits = p.word_bits() as usize;
    let mask = (1u64 << p.bits) - 1;
    // Bits after the samples of a row up to the next row (negative: the rows share a byte).
    let row_tail_bits = (p.row_bytes() * 8) as i64 - (w as i64) * i64::from(p.bits);
    let swap = usize::from(p.flags & FLAG_SWAP_PAIRS != 0);
    let padded = p.flags & FLAG_PAD_BYTE != 0;
    let interlaced = p.flags & FLAG_INTERLACE != 0;
    let first_field_rows = h.div_ceil(2);
    let (mut origin, mut segment_first_row) = (offset, 0usize);
    let mut carried = WordCursor::at(offset);
    for stored in 0..h {
        let r = if !interlaced {
            stored
        } else if stored < first_field_rows {
            2 * stored
        } else {
            2 * (stored - first_field_rows) + 1
        };
        if interlaced && p.flags & FLAG_SECOND_FIELD_MID_FILE != 0 && r == 1 {
            origin = second_field_start(file.len());
            segment_first_row = stored;
            carried = WordCursor::at(origin);
        }
        let start = row_start(origin, stored - segment_first_row, &p);
        let (resume, mut cur) = if padded {
            (carried.pos, std::mem::replace(&mut carried, WordCursor::at(0)))
        } else {
            (start.resume_byte, WordCursor { pos: start.byte, window: 0, avail: -(start.bit as i64) })
        };
        if resume > file.len() {
            return;
        }
        let base = r * w;
        let mut since_pad = 0;
        for c in 0..w {
            let v = cur.sample(file, p.bits, word_bits, mask);
            let d = c ^ swap;
            if d < w {
                if let Some(cell) = out.get_mut(base + d) {
                    *cell = v;
                }
            }
            since_pad += 1;
            if padded && since_pad == PAD_GROUP {
                since_pad = 0;
                cur.pos += 1;
            }
        }
        if padded {
            cur.avail -= row_tail_bits;
            carried = cur;
        }
    }
}

/// Decode MIPI RAW10 rows (`stride` bytes apart) into `out`.
pub fn mipi10_rows(src: &[u8], width: usize, height: usize, stride: usize, out: &mut [u16], parallel: bool) {
    if width == 0 || height == 0 {
        return;
    }
    let out = &mut out[..width * height];
    par::for_each_row_chunk(out, width, ROWS_PER_TASK, parallel, |first, chunk| {
        for (i, row) in chunk.chunks_exact_mut(width).enumerate() {
            let start = (first + i) * stride;
            if start > src.len() {
                break;
            }
            mipi10_row(&src[start..], row);
        }
    });
}

#[inline]
fn mipi10_group(b: &[u8; 5], o: &mut [u16; 4]) {
    let low = u16::from(b[4]);
    o[0] = (u16::from(b[0]) << 2) | (low & 3);
    o[1] = (u16::from(b[1]) << 2) | ((low >> 2) & 3);
    o[2] = (u16::from(b[2]) << 2) | ((low >> 4) & 3);
    o[3] = (u16::from(b[3]) << 2) | (low >> 6);
}

fn mipi10_row(src: &[u8], row: &mut [u16]) {
    let full = (row.len() / 4).min(src.len() / 5);
    for (o, b) in row.chunks_exact_mut(4).zip(src.chunks_exact(5)).take(full) {
        mipi10_group(b.try_into().unwrap(), o.try_into().unwrap());
    }
    // Groups at the end of the data or of the row: missing bytes read as zero.
    for (g, o) in row.chunks_mut(4).enumerate().skip(full) {
        let mut b = [0u8; 5];
        if let Some(s) = src.get(g * 5..) {
            let n = s.len().min(5);
            b[..n].copy_from_slice(&s[..n]);
        }
        let mut t = [0u16; 4];
        mipi10_group(&b, &mut t);
        o.copy_from_slice(&t[..o.len()]);
    }
}

/// Decode Android "loose" RAW10 rows into `out`.
pub fn loose10_rows(src: &[u8], width: usize, height: usize, out: &mut [u16], parallel: bool) {
    if width == 0 || height == 0 {
        return;
    }
    let stride = width.div_ceil(6) * 8;
    let out = &mut out[..width * height];
    par::for_each_row_chunk(out, width, ROWS_PER_TASK, parallel, |first, chunk| {
        for (i, row) in chunk.chunks_exact_mut(width).enumerate() {
            let start = (first + i) * stride;
            if start > src.len() {
                break;
            }
            let data = &src[start..];
            for (g, o) in row.chunks_mut(6).enumerate() {
                let word = match data.get(g * 8..g * 8 + 8) {
                    Some(b) => u64::from_le_bytes(b.try_into().unwrap()),
                    None => 0,
                };
                for (k, d) in o.iter_mut().enumerate() {
                    *d = ((word >> (10 * k)) & 0x3ff) as u16;
                }
            }
        }
    });
}

/// Decode 16-bit rows (either byte order, optional right shift, optional bottom-up) into `out`.
#[allow(clippy::too_many_arguments)]
pub fn u16_rows(
    src: &[u8],
    width: usize,
    height: usize,
    big_endian: bool,
    shift: u32,
    bottom_up: bool,
    out: &mut [u16],
    parallel: bool,
) {
    if width == 0 || height == 0 {
        return;
    }
    let shift = shift.min(15);
    let out = &mut out[..width * height];
    let order = if big_endian { PackOrder::MsbBytes } else { PackOrder::LsbBytes };
    // Samples `0..n` of `dst` from `data` (the stored bytes from the first of them on).
    let decode = |data: &[u8], dst: &mut [u16]| {
        let n = dst.len().min(data.len() / 2);
        let dst = &mut dst[..n];
        unpack_row(&data[..n * 2], dst, 16, order, true);
        if shift != 0 {
            for v in dst.iter_mut() {
                *v >>= shift;
            }
        }
    };
    par::for_each_row_chunk(out, width, ROWS_PER_TASK, parallel, |first, chunk| {
        if !bottom_up {
            // Top-down rows are stored back to back, as they are in `chunk`: decoding the
            // chunk at once gives the same samples, and one larger copy runs faster than
            // one per row.
            if let Some(data) = src.get(first * 2 * width..) {
                decode(data, chunk);
            }
            return;
        }
        for (i, row) in chunk.chunks_exact_mut(width).enumerate() {
            let sy = height - 1 - (first + i);
            if let Some(data) = src.get(sy * 2 * width..) {
                decode(data, row);
            }
        }
    });
}
