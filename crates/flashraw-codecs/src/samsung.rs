//! Samsung SRW compressions.
//!
//! | TIFF compression | name | layout |
//! |---|---|---|
//! | 32770 + tag 40976 | V0 | per-row offsets; 16-sample blocks with a direction bit and four adaptive bit lengths; rows stored as 2x2-transposed pairs |
//! | 32772 | V1 | single predictive Huffman stream (fixed 10-bit table), see [`crate::nikon_huffman`] |
//! | 32773 | V2 | rows aligned to 16 bytes; 16-sample blocks with motion-compensated prediction from previous rows, adaptive bit lengths and a quantisation scale |
//!
//! V0 and V2 read bits MSB-first from little-endian 32-bit words, fetching a new word
//! only when the buffered bits run out ([`Ph1Bits`]); V2 relies on this exact fetch
//! pattern for its row alignment.

use crate::nikon_huffman::{DirectHuff, PredictiveParams, decode_predictive};
use flashraw_core::{Error, Result};

/// MSB-first reader over little-endian 32-bit words with lazy word fetches.
/// Reading past the end yields all-ones words and is flagged.
#[derive(Clone)]
pub struct Ph1Bits<'a> {
    data: &'a [u8],
    /// Byte position of the next word to fetch.
    pos: usize,
    buf: u64,
    vbits: u32,
    overrun: bool,
}

impl<'a> Ph1Bits<'a> {
    pub fn new(data: &'a [u8], pos: usize) -> Self {
        Ph1Bits { data, pos, buf: 0, vbits: 0, overrun: false }
    }

    /// Position of the next word fetch (bytes consumed in whole words).
    pub fn pos(&self) -> usize {
        self.pos
    }

    /// Discard buffered bits and continue fetching at `pos`.
    pub fn reset_at(&mut self, pos: usize) {
        self.pos = pos;
        self.buf = 0;
        self.vbits = 0;
    }

    pub fn overrun(&self) -> bool {
        self.overrun
    }

    #[inline(always)]
    fn fetch(&mut self) {
        let w = match self.data.get(self.pos..self.pos + 4) {
            Some(s) => u32::from_le_bytes([s[0], s[1], s[2], s[3]]),
            None => {
                self.overrun = true;
                u32::MAX
            }
        };
        self.pos += 4;
        self.buf = (self.buf << 32) | w as u64;
        self.vbits += 32;
    }

    /// Read `n <= 32` bits.
    #[inline(always)]
    pub fn get(&mut self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        let n = n.min(32);
        if self.vbits < n {
            self.fetch();
        }
        let v = ((self.buf << (64 - self.vbits)) >> (64 - n)) as u32;
        self.vbits -= n;
        v
    }
}

/// Sequential reference implementation of Samsung V0 (see [`decode_v0`]); kept as the
/// specification the two-phase decoder is tested against.
pub fn decode_v0_reference(
    src: &[u8],
    row_offsets: &[u32],
    width: usize,
    height: usize,
    out: &mut [u16],
) -> Result<()> {
    if out.len() < width * height || row_offsets.len() < height {
        return Err(Error::InvalidArgument("samsung v0: buffer/offsets too small".into()));
    }
    let max_index = width * (height + 7);
    let raw_len = width * height;
    for (row, &row_offset) in row_offsets.iter().enumerate().take(height) {
        let mut bits = Ph1Bits::new(src, row_offset as usize);
        let mut len = [if row < 2 { 7i32 } else { 4 }; 4];
        let mut col = 0usize;
        while col < width {
            let dir = bits.get(1);
            let mut op = [0u32; 4];
            for o in op.iter_mut() {
                *o = bits.get(2);
            }
            for c in 0..4 {
                match op[c] {
                    3 => len[c] = bits.get(4) as i32,
                    2 => len[c] -= 1,
                    1 => len[c] += 1,
                    _ => {}
                }
            }
            // Even samples first, then odd ones.
            for c in (0..16).step_by(2).chain((1..16).step_by(2)) {
                let i = len[((c & 1) << 1) | (c >> 3)];
                let dest = row * width + col + c;
                // Reference predictor: same column 1 or 2 rows up (dir), else the
                // block's left neighbours (col - 2 for even, col - 1 for odd samples).
                let src_idx: Option<usize> = if dir != 0 {
                    let up = if c & 1 == 0 { 1 } else { 2 };
                    row.checked_sub(up).map(|r| r * width + col + c)
                } else if col > 0 {
                    Some(row * width + col - if c & 1 == 0 { 2 } else { 1 })
                } else {
                    None
                };
                let pred = match src_idx {
                    Some(s) if s < max_index && dest < max_index => {
                        if s < raw_len {
                            out[s] as i32
                        } else {
                            0
                        }
                    }
                    Some(_) => {
                        // Reference skips the write (reports a data error).
                        continue;
                    }
                    None if dir != 0 => continue,
                    None => 128,
                };
                let d = if i > 0 {
                    let i = i.min(32) as u32;
                    let v = bits.get(i);
                    ((v << (32 - i)) as i32) >> (32 - i)
                } else {
                    0
                };
                if dest < raw_len {
                    out[dest] = (d.wrapping_add(pred)) as u16;
                }
            }
            col += 16;
        }
    }
    // Undo the 2x2 transposition of each even/odd row pair.
    let mut row = 0;
    while row + 1 < height {
        let mut col = 0;
        while col + 1 < width {
            out.swap(row * width + col + 1, (row + 1) * width + col);
            col += 2;
        }
        row += 2;
    }
    Ok(())
}

/// Per-sample reconstruction rule produced by the V0 parser.
#[derive(Clone, Copy, Default)]
struct V0Op {
    /// Residual to add to the prediction.
    diff: i32,
    /// 0 = left neighbour of the block, 1 = rows above, 2 = constant 128, 3 = skipped.
    kind: u8,
}

/// Parse one V0 row (bitstream only; no pixel values needed) into per-column ops.
fn parse_v0_row(src: &[u8], offset: usize, row: usize, width: usize, max_index: usize, ops: &mut [V0Op]) {
    let mut bits = Ph1Bits::new(src, offset);
    let mut len = [if row < 2 { 7i32 } else { 4 }; 4];
    let mut col = 0usize;
    while col < width {
        let dir = bits.get(1);
        let mut op = [0u32; 4];
        for o in op.iter_mut() {
            *o = bits.get(2);
        }
        for c in 0..4 {
            match op[c] {
                3 => len[c] = bits.get(4) as i32,
                2 => len[c] -= 1,
                1 => len[c] += 1,
                _ => {}
            }
        }
        for c in (0..16).step_by(2).chain((1..16).step_by(2)) {
            let i = len[((c & 1) << 1) | (c >> 3)];
            // Skip rules depend only on geometry:
            // a skipped sample consumes no residual bits.
            let dest = row * width + col + c;
            let src_idx: Option<usize> = if dir != 0 {
                let up = if c & 1 == 0 { 1 } else { 2 };
                row.checked_sub(up).map(|r| r * width + col + c)
            } else if col > 0 {
                Some(row * width + col - if c & 1 == 0 { 2 } else { 1 })
            } else {
                None
            };
            let kind = match src_idx {
                Some(s) if s < max_index && dest < max_index => {
                    if dir != 0 {
                        1
                    } else {
                        0
                    }
                }
                Some(_) => 3,
                None if dir != 0 => 3,
                None => 2,
            };
            if kind == 3 {
                if col + c < ops.len() {
                    ops[col + c] = V0Op { diff: 0, kind };
                }
                continue;
            }
            let d = if i > 0 {
                let i = i.min(32) as u32;
                let v = bits.get(i);
                ((v << (32 - i)) as i32) >> (32 - i)
            } else {
                0
            };
            if col + c < ops.len() {
                ops[col + c] = V0Op { diff: d, kind };
            }
        }
        col += 16;
    }
}

/// Decode Samsung V0 (compression 32770 with a row offset table).
///
/// `row_offsets[row]` is the byte offset of each row's data within `src`. Rows'
/// bitstreams are independent, but prediction reads reconstructed pixels of the rows
/// above, so decoding runs in two phases per band of rows: parallel bitstream parsing
/// into residuals, then a cheap sequential reconstruction. Output is identical to
/// [`decode_v0_reference`].
pub fn decode_v0(src: &[u8], row_offsets: &[u32], width: usize, height: usize, out: &mut [u16]) -> Result<()> {
    decode_v0_with(src, row_offsets, width, height, out, true)
}

/// [`decode_v0`] with explicit control over parallel parsing.
pub fn decode_v0_with(
    src: &[u8],
    row_offsets: &[u32],
    width: usize,
    height: usize,
    out: &mut [u16],
    parallel: bool,
) -> Result<()> {
    if out.len() < width * height || row_offsets.len() < height {
        return Err(Error::InvalidArgument("samsung v0: buffer/offsets too small".into()));
    }
    let max_index = width * (height + 7);
    let raw_len = width * height;
    let cols = width.div_ceil(16) * 16;
    const BAND: usize = 64;
    let mut row0 = 0usize;
    while row0 < height {
        let rows: Vec<usize> = (row0..(row0 + BAND).min(height)).collect();
        let parsed: Vec<Vec<V0Op>> = flashraw_core::par::map(rows, parallel, |row| {
            let mut ops = vec![V0Op::default(); cols];
            parse_v0_row(src, row_offsets[row] as usize, row, width, max_index, &mut ops);
            ops
        });
        for (k, ops) in parsed.iter().enumerate() {
            let row = row0 + k;
            let mut col = 0usize;
            while col < width {
                for c in 0..16 {
                    let dest = row * width + col + c;
                    let op = ops[col + c];
                    let pred = match op.kind {
                        1 => {
                            let up = if c & 1 == 0 { 1 } else { 2 };
                            let s = (row - up) * width + col + c;
                            if s < raw_len { out[s] as i32 } else { 0 }
                        }
                        0 => {
                            let s = row * width + col - if c & 1 == 0 { 2 } else { 1 };
                            if s < raw_len { out[s] as i32 } else { 0 }
                        }
                        2 => 128,
                        _ => continue,
                    };
                    if dest < raw_len {
                        out[dest] = (op.diff.wrapping_add(pred)) as u16;
                    }
                }
                col += 16;
            }
        }
        row0 += BAND;
    }
    // Undo the 2x2 transposition of each even/odd row pair.
    let mut row = 0;
    while row + 1 < height {
        let mut col = 0;
        while col + 1 < width {
            out.swap(row * width + col + 1, (row + 1) * width + col);
            col += 2;
        }
        row += 2;
    }
    Ok(())
}

/// The fixed V1 Huffman table (`code_len << 8 | diff_len`), 10-bit direct lookup.
const V1_TABLE: [u16; 14] =
    [0x304, 0x307, 0x206, 0x205, 0x403, 0x600, 0x709, 0x80a, 0x90b, 0xa0c, 0xa0d, 0x501, 0x408, 0x402];

fn v1_table() -> DirectHuff {
    let mut direct = Vec::with_capacity(1024);
    for &e in &V1_TABLE {
        for _ in 0..(1024 >> (e >> 8)) {
            direct.push(e);
        }
    }
    direct.resize(1024, 0);
    DirectHuff::from_direct(10, direct)
}

/// Decode Samsung V1 (compression 32772).
pub fn decode_v1(src: &[u8], width: usize, height: usize, out: &mut [u16]) -> Result<bool> {
    let t = v1_table();
    let p = PredictiveParams { width, rows: height, tables: &[(0, &t)], vpred: [[0; 2]; 2] };
    Ok(decode_predictive(src, &p, out, width, |h| h)?.is_dead())
}

/// Decode Samsung V2 (compression 32773). `src` starts at the image data.
pub fn decode_v2(src: &[u8], width: usize, height: usize, out: &mut [u16]) -> Result<()> {
    if height < 436 || width < 646 {
        return Err(Error::corrupt("Samsung V2 image too small"));
    }
    if out.len() < width * height {
        return Err(Error::InvalidArgument("samsung v2: output too small".into()));
    }
    let mut bits = Ph1Bits::new(src, 0);
    bits.get(16);
    bits.get(4);
    let bit_depth = bits.get(4) + 1;
    let datamax = (1i32 << bit_depth) - 1;
    bits.get(8);
    bits.get(16);
    bits.get(16);
    bits.get(16);
    bits.get(4);
    let optflags = bits.get(4);
    bits.get(16);
    bits.get(8);
    bits.get(2);
    let init_val = bits.get(14) as u16;
    let hdr = V2Hdr { bit_depth, datamax, optflags, init_val };
    for row in 0..height {
        // Each row starts at the next 16-byte boundary (relative to the data start).
        let p = bits.pos();
        bits.reset_at(p + ((16 - p % 16) % 16));
        v2_row(&mut bits, &hdr, row, width, height, out)?;
        if bits.overrun() {
            return Err(Error::corrupt(format!("Samsung V2 data truncated at row {row}/{height}")));
        }
    }
    Ok(())
}

struct V2Hdr {
    bit_depth: u32,
    datamax: i32,
    optflags: u32,
    init_val: u16,
}

/// Position of difference `i` within a 16-sample block, for even and odd rows.
const V2_ORDER_EVEN: [u8; 16] = {
    let mut o = [0u8; 16];
    let mut i = 0;
    while i < 16 {
        o[i] = (((i & 7) << 1) + (i >> 3)) as u8;
        i += 1;
    }
    o
};
const V2_ORDER_ODD: [u8; 16] = {
    let mut o = [0u8; 16];
    let mut i = 0;
    while i < 16 {
        o[i] = (((i & 7) << 1) + 1 - (i >> 3)) as u8;
        i += 1;
    }
    o
};

/// Decode row `row` of a V2 image (motion-compensated blocks of 16 samples).
#[inline(always)]
fn v2_row(bits: &mut Ph1Bits, h: &V2Hdr, row: usize, width: usize, height: usize, out: &mut [u16]) -> Result<()> {
    let V2Hdr { bit_depth, datamax, optflags, init_val } = *h;
    let total = (width * height) as i64;
    const MOTION_OFFSET: [i64; 7] = [-4, -2, -2, 0, 0, 2, 4];
    const MOTION_AVERAGE: [bool; 7] = [false, false, true, false, true, false, false];
    const SCALEVALS: [i32; 3] = [0, -2, 2];
    let rowstart = row * width;
    let mut motion = 7u32;
    let mut scale: i32 = 0;
    let mut mode = [[if row < 2 { 7u32 } else { 4 }; 2]; 3];
    let mut col = 0usize;
    while col + 15 < width {
        if optflags & 4 == 0 && col & 63 == 0 {
            let i = bits.get(2);
            if i < 3 {
                scale += SCALEVALS[i as usize];
            } else {
                scale = bits.get(12) as i32;
            }
        }
        if optflags & 2 != 0 {
            motion = if bits.get(1) != 0 { 3 } else { 7 };
        } else if bits.get(1) == 0 {
            motion = bits.get(3);
        }
        // Stages as plain arrays (prediction, bit reading, shuffle, scaled add) so the
        // arithmetic vectorises and bit reading is not interleaved with stores.
        let mut base = [0u16; 16];
        if motion == 7 {
            // Repeat the previous two samples (the initial value at the row start).
            let (a, b) =
                if col == 0 { (init_val, init_val) } else { (out[rowstart + col - 2], out[rowstart + col - 1]) };
            for px in base.chunks_exact_mut(2) {
                px[0] = a;
                px[1] = b;
            }
        } else {
            if row < 2 {
                return Err(Error::corrupt("Samsung V2: motion prediction on the first rows"));
            }
            let m = motion as usize;
            let avg = MOTION_AVERAGE[m];
            // Red/blue samples (row + i odd) use the same colour two rows up; green ones the
            // diagonal green one row up. Indices are checked once for the whole block.
            let (up2, up1) = (((row - 2) * width) as i64, ((row - 1) * width) as i64);
            let mut idx = [0i64; 16];
            for (i, x) in idx.iter_mut().enumerate() {
                let c = (col + i) as i64 + MOTION_OFFSET[m];
                *x = if (row + i) & 1 == 1 { up2 + c } else { up1 + c + 1 - 2 * (i as i64 & 1) };
            }
            let (lo, hi) = idx.iter().fold((i64::MAX, i64::MIN), |(a, b), &x| (a.min(x), b.max(x)));
            if lo < 0 || hi + if avg { 2 } else { 0 } >= total {
                return Err(Error::corrupt("Samsung V2: motion reference out of range"));
            }
            for (b, &x) in base.iter_mut().zip(&idx) {
                let r = x as usize;
                *b = if avg { ((out[r] as u32 + out[r + 2] as u32 + 1) >> 1) as u16 } else { out[r] };
            }
        }
        let mut diff_bits = [0u32; 4];
        if optflags & 1 != 0 || bits.get(1) == 0 {
            let mut flags = [0u32; 4];
            for f in flags.iter_mut() {
                *f = bits.get(2);
            }
            for i in 0..4 {
                let colornum = if row % 2 == 1 { i >> 1 } else { ((i >> 1) + 2) % 3 };
                diff_bits[i] = match flags[i] {
                    0 => mode[colornum][0],
                    1 => mode[colornum][0].wrapping_add(1),
                    2 => mode[colornum][0].wrapping_sub(1),
                    _ => bits.get(4),
                };
                mode[colornum][0] = mode[colornum][1];
                mode[colornum][1] = diff_bits[i];
                if diff_bits[i] > bit_depth + 1 {
                    return Err(Error::corrupt("Samsung V2: invalid difference length"));
                }
            }
        }
        // Sample order inside the block: even positions then odd ones, shifted by the
        // row parity. Positions stay inside `col..col + 16`, so inside the row.
        let order = if row % 2 == 1 { &V2_ORDER_ODD } else { &V2_ORDER_EVEN };
        let mut raw = [0i32; 16];
        for (i, &o) in order.iter().enumerate() {
            let len = diff_bits[i >> 2];
            if len != 0 {
                let sign_mask = 1i32 << (len - 1);
                let v = bits.get(len) as i32;
                raw[o as usize] = (v ^ sign_mask).wrapping_sub(sign_mask);
            }
        }
        let mul = scale.wrapping_mul(2).wrapping_add(1);
        let blk = &mut out[rowstart + col..rowstart + col + 16];
        for ((dst, &b), &r) in blk.iter_mut().zip(&base).zip(&raw) {
            *dst = (b as i32 + r.wrapping_mul(mul).wrapping_add(scale)).clamp(0, datamax) as u16;
        }
        col += 16;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ph1_reader_lazy_fetch() {
        let data = [0x78u8, 0x56, 0x34, 0x12, 0xEF, 0xCD, 0xAB, 0x90];
        let mut b = Ph1Bits::new(&data, 0);
        assert_eq!(b.get(8), 0x12);
        assert_eq!(b.pos(), 4);
        assert_eq!(b.get(24), 0x345678);
        assert_eq!(b.pos(), 4);
        assert_eq!(b.get(4), 0x9);
        assert_eq!(b.pos(), 8);
        assert!(!b.overrun());
        b.get(32);
        b.get(32);
        assert!(b.overrun());
    }

    #[test]
    fn v1_table_is_complete() {
        let total: usize = V1_TABLE.iter().map(|&e| 1024 >> (e >> 8)).sum();
        assert_eq!(total, 1024);
        let mut out = vec![0u16; 64 * 4];
        decode_v1(&[0xA5; 300], 64, 4, &mut out).unwrap();
    }

    #[test]
    fn v0_constant_rows() {
        // All-zero stream: dir 0, ops 0 → every block predicts from the left (128 at col 0).
        let width = 32;
        let height = 4;
        let src = vec![0u8; 4096];
        let offsets: Vec<u32> = (0..height as u32).map(|r| r * 64).collect();
        let mut out = vec![0u16; width * height];
        decode_v0(&src, &offsets, width, height, &mut out).unwrap();
        assert!(out.iter().all(|&v| v == 128));
    }

    #[test]
    fn v0_two_phase_matches_reference() {
        let mut x: u64 = 0x1234_5678_9abc_def1;
        for (width, height) in [(32usize, 6usize), (48, 9), (100, 13), (16, 2)] {
            let src: Vec<u8> = (0..width * height * 8 + 64)
                .map(|_| {
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    x as u8
                })
                .collect();
            let row_bytes = width * 4;
            let offsets: Vec<u32> = (0..height as u32).map(|r| r * row_bytes as u32).collect();
            let mut a = vec![0u16; width * height];
            let mut b = vec![7u16; width * height];
            let mut c = vec![7u16; width * height];
            decode_v0_reference(&src, &offsets, width, height, &mut a).unwrap();
            // Untouched samples keep their initial value in both; normalise via a zeroed buffer.
            let mut b0 = vec![0u16; width * height];
            decode_v0_with(&src, &offsets, width, height, &mut b0, true).unwrap();
            let mut c0 = vec![0u16; width * height];
            decode_v0_with(&src, &offsets, width, height, &mut c0, false).unwrap();
            assert_eq!(a, b0, "{width}x{height} parallel");
            assert_eq!(a, c0, "{width}x{height} sequential");
            let _ = (&mut b, &mut c);
        }
    }

    #[test]
    fn v2_rejects_small_and_garbage() {
        let mut out = vec![0u16; 10];
        assert!(decode_v2(&[0; 100], 10, 1, &mut out).is_err());
        let (w, h) = (656usize, 440usize);
        let mut out = vec![0u16; w * h];
        // Truncated garbage must error out, never panic.
        let r = decode_v2(&[0x5Au8; 2000], w, h, &mut out);
        assert!(r.is_err());
    }
}
