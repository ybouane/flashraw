//! Sigma / Foveon X3F entropy coders.
//!
//! X3F raw images come in three families:
//!
//! - **TRUE** (DP1/DP2/SD14/SD15 "TRUE I/II", Merrill "TRUE II", Quattro "TRUE III"):
//!   each of the three sensor layers is an independent bit stream (planes start at
//!   16-byte aligned offsets) coded with one shared prefix code whose symbols are
//!   the bit length of a JPEG-style difference. Prediction works on 2x2 phases: the
//!   first two columns of a row predict from the same phase two rows above, all
//!   other samples from the same phase two columns to the left. Planes decode in
//!   parallel; within a plane the stream is strictly sequential.
//! - **Huffman 10-bit** (SD9/SD10/SD14, Polaroid x530): interleaved R,G,B
//!   difference codes per row (independent rows via a row-offset table), each
//!   symbol mapped through a 1024-entry difference table. Running sums are 16-bit
//!   signed; if any sum goes negative the whole image is offset so the minimum is
//! - **Uncompressed "simple"** variant of the above: one 32-bit word per pixel
//!   holding three 10-bit mapping-table indices.
//!
//! Prefix codes in X3F are not canonical (they are given as explicit codes), so
//! [`PrefixCode`] builds a binary tree plus a 12-bit lookup table on top of it.

use flashraw_core::bits::{BitPumpMsb, BitRead};
use flashraw_core::{Error, Result};

const LUT_BITS: u32 = 12;
const NONE: u32 = u32::MAX;

// LUT entry layout: bits 0..5 length, bits 6..8 kind, bits 8..32 payload (value or node index).
const KIND_LEAF: u32 = 1 << 6;
const KIND_NODE: u32 = 2 << 6;
const KIND_MASK: u32 = 3 << 6;

/// A (possibly non-canonical, possibly incomplete) binary prefix code.
#[derive(Debug, Clone)]
pub struct PrefixCode {
    /// Tree nodes: `[child0, child1]`, `NONE` when absent.
    nodes: Vec<[u32; 2]>,
    /// Leaf value per node (`NONE` for internal nodes).
    values: Vec<u32>,
    lut: Vec<u32>,
}

impl PrefixCode {
    /// Build from `(code, length, value)` triples; `code` is right-aligned.
    pub fn new(codes: &[(u32, u32, u32)]) -> Result<Self> {
        let mut nodes = vec![[NONE, NONE]];
        let mut values = vec![NONE];
        for &(code, len, value) in codes {
            if len == 0 || len > 27 {
                return Err(Error::corrupt(format!("X3F prefix code with invalid length {len}")));
            }
            if value >= 1 << 24 {
                return Err(Error::corrupt("X3F prefix code value out of range"));
            }
            let mut n = 0usize;
            for i in (0..len).rev() {
                let bit = ((code >> i) & 1) as usize;
                let next = nodes[n][bit];
                n = if next == NONE {
                    nodes.push([NONE, NONE]);
                    values.push(NONE);
                    let idx = nodes.len() - 1;
                    nodes[n][bit] = idx as u32;
                    idx
                } else {
                    next as usize
                };
            }
            values[n] = value;
        }
        if nodes[0] == [NONE, NONE] {
            return Err(Error::corrupt("X3F prefix code without codes"));
        }
        let mut code = PrefixCode { nodes, values, lut: Vec::new() };
        code.build_lut();
        Ok(code)
    }

    fn is_leaf(&self, n: usize) -> bool {
        self.nodes[n] == [NONE, NONE]
    }

    fn build_lut(&mut self) {
        let mut lut = vec![0u32; 1 << LUT_BITS];
        for (idx, e) in lut.iter_mut().enumerate() {
            let mut n = 0usize;
            let mut len = 0;
            *e = loop {
                if self.is_leaf(n) {
                    // A terminal node without a value is an unusable code.
                    break if self.values[n] == NONE { 0 } else { len | KIND_LEAF | (self.values[n] << 8) };
                }
                if len == LUT_BITS {
                    break LUT_BITS | KIND_NODE | ((n as u32) << 8);
                }
                let bit = (idx >> (LUT_BITS - 1 - len)) & 1;
                let next = self.nodes[n][bit];
                if next == NONE {
                    break 0;
                }
                n = next as usize;
                len += 1;
            };
        }
        self.lut = lut;
    }

    /// Decode one symbol.
    #[inline(always)]
    pub fn decode<B: BitRead>(&self, bits: &mut B) -> Result<u32> {
        bits.fill();
        let e = self.lut[bits.peek_no_fill(LUT_BITS) as usize];
        match e & KIND_MASK {
            KIND_LEAF => {
                bits.skip_no_fill(e & 0x3F);
                Ok(e >> 8)
            }
            KIND_NODE => {
                bits.skip_no_fill(LUT_BITS);
                self.decode_slow(bits, (e >> 8) as usize)
            }
            _ => Err(Error::corrupt("invalid X3F prefix code")),
        }
    }

    #[cold]
    fn decode_slow<B: BitRead>(&self, bits: &mut B, mut n: usize) -> Result<u32> {
        while !self.is_leaf(n) {
            let next = self.nodes[n][bits.get(1) as usize];
            if next == NONE {
                return Err(Error::corrupt("invalid X3F prefix code"));
            }
            n = next as usize;
        }
        match self.values[n] {
            NONE => Err(Error::corrupt("invalid X3F prefix code")),
            v => Ok(v),
        }
    }
}

/// Parsed header of a TRUE-engine raw image section (after the 28-byte image header).
#[derive(Debug, Clone)]
pub struct TrueHeader {
    /// Quattro plane sizes `(columns, rows)` (bottom, middle, top) when present.
    pub planes: Option<[(u32, u32); 3]>,
    pub seeds: [u16; 3],
    /// `(code_size, code)` pairs; the symbol of entry `i` is `i` (the diff bit length).
    pub table: Vec<(u8, u8)>,
    pub plane_sizes: [u32; 3],
    /// Offset of the first plane's data within the parsed slice.
    pub data_offset: usize,
}

fn rd16(d: &[u8], p: &mut usize) -> Result<u16> {
    let b = d.get(*p..*p + 2).ok_or_else(|| Error::corrupt("truncated X3F TRUE header"))?;
    *p += 2;
    Ok(u16::from_le_bytes([b[0], b[1]]))
}

fn rd32(d: &[u8], p: &mut usize) -> Result<u32> {
    let b = d.get(*p..*p + 4).ok_or_else(|| Error::corrupt("truncated X3F TRUE header"))?;
    *p += 4;
    Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

impl TrueHeader {
    /// Parse from the image payload (the bytes following the 28-byte `SECi` header).
    pub fn parse(d: &[u8], quattro: bool) -> Result<Self> {
        let mut p = 0usize;
        let planes = if quattro {
            let mut pl = [(0u32, 0u32); 3];
            for e in pl.iter_mut() {
                let c = rd16(d, &mut p)? as u32;
                let r = rd16(d, &mut p)? as u32;
                *e = (c, r);
            }
            Some(pl)
        } else {
            None
        };
        let seeds = [rd16(d, &mut p)?, rd16(d, &mut p)?, rd16(d, &mut p)?];
        let _unknown = rd16(d, &mut p)?;
        let mut table = Vec::new();
        loop {
            let b = d.get(p..p + 2).ok_or_else(|| Error::corrupt("truncated X3F TRUE code table"))?;
            p += 2;
            table.push((b[0], b[1]));
            if b[0] == 0 {
                break;
            }
            if table.len() > 256 {
                return Err(Error::corrupt("X3F TRUE code table too long"));
            }
        }
        if quattro {
            let _unknown = rd32(d, &mut p)?;
        }
        let plane_sizes = [rd32(d, &mut p)?, rd32(d, &mut p)?, rd32(d, &mut p)?];
        Ok(TrueHeader { planes, seeds, table, plane_sizes, data_offset: p })
    }

    /// The prefix code (symbol = diff bit length).
    pub fn code(&self) -> Result<PrefixCode> {
        let codes: Vec<(u32, u32, u32)> = self
            .table
            .iter()
            .enumerate()
            .filter(|(_, (len, _))| *len != 0)
            .map(|(i, &(len, code))| {
                let len = len as u32;
                let c = if len <= 8 { (code as u32 >> (8 - len)) & 0xFF } else { u32::MAX };
                (c, len, i as u32)
            })
            .collect();
        if codes.iter().any(|&(c, _, _)| c == u32::MAX) {
            return Err(Error::corrupt("X3F TRUE code longer than 8 bits"));
        }
        PrefixCode::new(&codes)
    }

    /// Byte ranges `(start, end)` of each plane within `data` (relative to the data start).
    /// A plane's stream may legally run into the following plane's padding, so `end` is
    /// the end of all image data.
    pub fn plane_ranges(&self, data_len: usize) -> Result<[(usize, usize); 3]> {
        let mut out = [(0usize, data_len); 3];
        let mut off = 0usize;
        for (i, &s) in self.plane_sizes.iter().enumerate() {
            out[i].0 = off;
            off = off
                .checked_add((s as usize).div_ceil(16) * 16)
                .ok_or_else(|| Error::corrupt("X3F TRUE plane sizes overflow"))?;
        }
        if off > data_len {
            return Err(Error::corrupt("X3F TRUE planes exceed section size"));
        }
        Ok(out)
    }
}

/// Decode one TRUE plane of `cols x rows` samples into `out` (`out_cols` samples per row,
/// `out_cols <= cols`; extra columns on the right are decoded and discarded).
pub fn decode_true_plane(
    code: &PrefixCode,
    data: &[u8],
    seed: u16,
    cols: usize,
    rows: usize,
    out_cols: usize,
    out: &mut [u16],
) -> Result<()> {
    if out_cols > cols || out.len() < out_cols * rows {
        return Err(Error::corrupt("X3F TRUE plane geometry mismatch"));
    }
    let mut bits = BitPumpMsb::new(data);
    let mut row_start = [[seed as i32; 2]; 2];
    for (r, orow) in out.chunks_exact_mut(out_cols.max(1)).take(rows).enumerate() {
        let odd_row = r & 1;
        let mut acc = [0i32; 2];
        for col in 0..cols {
            let len = code.decode(&mut bits)?;
            let diff = if len == 0 {
                0
            } else {
                if len > 31 {
                    return Err(Error::corrupt("X3F TRUE difference too long"));
                }
                let v = read_bits(&mut bits, len);
                if v >> (len - 1) == 0 { v as i32 - ((1i64 << len) - 1) as i32 } else { v as i32 }
            };
            let phase = col & 1;
            let value = if col < 2 {
                let v = row_start[odd_row][phase].wrapping_add(diff);
                row_start[odd_row][phase] = v;
                v
            } else {
                acc[phase].wrapping_add(diff)
            };
            acc[phase] = value;
            if col < out_cols {
                orow[col] = value as u16;
            }
        }
        if bits.exhausted() {
            return Err(Error::corrupt("X3F TRUE plane truncated"));
        }
    }
    Ok(())
}

#[inline(always)]
fn read_bits<B: BitRead>(bits: &mut B, n: u32) -> u32 {
    // `fill` guarantees at least 32 buffered bits and `n <= 31`.
    bits.get(n)
}

/// Code table of the 10-bit Huffman format: entries are `length << 27 | code`, the
/// symbol of entry `i` is `mapping[i]` when a full mapping table is present, else `i`.
pub fn huffman_code(table: &[u32], mapping: &[u16]) -> Result<PrefixCode> {
    let codes: Vec<(u32, u32, u32)> = table
        .iter()
        .enumerate()
        .filter(|(_, e)| **e != 0)
        .map(|(i, &e)| {
            let v = if mapping.len() == table.len() { mapping[i] as u32 } else { i as u32 };
            (e & 0x07FF_FFFF, (e >> 27) & 0x1F, v)
        })
        .collect();
    PrefixCode::new(&codes)
}

/// Decode a row-indexed 10-bit Huffman image (3 interleaved colours) into `out`
/// (`cols * rows * 3` samples). `data` is the entropy-coded area, `row_offsets` the
/// per-row start offsets into it.
pub fn decode_huffman(
    code: &PrefixCode,
    data: &[u8],
    row_offsets: &[u32],
    cols: usize,
    rows: usize,
    out: &mut [u16],
    parallel: bool,
) -> Result<()> {
    let rl = cols * 3;
    if row_offsets.len() < rows || out.len() < rl * rows || rl == 0 {
        return Err(Error::corrupt("X3F Huffman geometry mismatch"));
    }
    let errors = std::sync::Mutex::new(None);
    let minimum = std::sync::atomic::AtomicI32::new(0);
    // Pass 1: 16-bit running sums starting at 0, stored as raw two's complement.
    flashraw_core::par::for_each_row_chunk(&mut out[..rl * rows], rl, 16, parallel, |first, chunk| {
        let mut local_min = 0i32;
        for (i, row) in chunk.chunks_exact_mut(rl).enumerate() {
            let r = first + i;
            let off = row_offsets[r] as usize;
            if off >= data.len() {
                *errors.lock().unwrap() = Some(Error::corrupt("X3F Huffman row offset out of range"));
                return;
            }
            let mut bits = BitPumpMsb::new(&data[off..]);
            let mut c = [0i16; 3];
            for px in row.chunks_exact_mut(3) {
                for (k, v) in px.iter_mut().enumerate() {
                    match code.decode(&mut bits) {
                        Ok(d) => c[k] = c[k].wrapping_add(d as i16),
                        Err(e) => {
                            *errors.lock().unwrap() = Some(e);
                            return;
                        }
                    }
                    local_min = local_min.min(c[k] as i32);
                    *v = c[k] as u16;
                }
            }
            if bits.exhausted() {
                *errors.lock().unwrap() = Some(Error::corrupt("X3F Huffman row truncated"));
                return;
            }
        }
        minimum.fetch_min(local_min, std::sync::atomic::Ordering::Relaxed);
    });
    if let Some(e) = errors.into_inner().unwrap() {
        return Err(e);
    }
    // Pass 2: offset everything so the most negative sum becomes 0, clamp negatives.
    let offset = (-minimum.into_inner()) as i16;
    flashraw_core::par::for_each_row_chunk(&mut out[..rl * rows], rl, 64, parallel, |_, chunk| {
        for v in chunk.iter_mut() {
            let s = (*v as i16).wrapping_add(offset);
            *v = if s < 0 { 0 } else { s as u16 };
        }
    });
    Ok(())
}

/// Decode the uncompressed variant: one little-endian 32-bit word per pixel holding
/// three `bits`-bit indices into `mapping` (differences), running sums per colour.
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
        return Err(Error::corrupt(format!("X3F simple coding with {bits} bits")));
    }
    let rl = cols * 3;
    if row_stride < cols * 4 || out.len() < rl * rows || rl == 0 {
        return Err(Error::corrupt("X3F uncompressed geometry mismatch"));
    }
    let needed = (rows - 1).checked_mul(row_stride).and_then(|v| v.checked_add(cols * 4));
    if needed.is_none_or(|n| n > data.len()) {
        return Err(Error::corrupt("X3F uncompressed data truncated"));
    }
    let mask = (1u32 << bits) - 1;
    flashraw_core::par::for_each_row_chunk(&mut out[..rl * rows], rl, 32, parallel, |first, chunk| {
        for (i, row) in chunk.chunks_exact_mut(rl).enumerate() {
            let src = &data[(first + i) * row_stride..];
            let mut c = [0u16; 3];
            for (px, w) in row.chunks_exact_mut(3).zip(src.chunks_exact(4)) {
                let val = u32::from_le_bytes([w[0], w[1], w[2], w[3]]);
                for k in 0..3 {
                    let idx = ((val >> (k as u32 * bits)) & mask) as usize;
                    let d = if mapping.is_empty() { idx as u16 } else { mapping.get(idx).copied().unwrap_or(0) };
                    c[k] = c[k].wrapping_add(d);
                    px[k] = if (c[k] as i16) > 0 { c[k] } else { 0 };
                }
            }
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// MSB-first bit writer for synthetic streams.
    struct W {
        out: Vec<u8>,
        acc: u64,
        n: u32,
    }
    impl W {
        fn new() -> Self {
            W { out: Vec::new(), acc: 0, n: 0 }
        }
        fn put(&mut self, v: u32, len: u32) {
            for i in (0..len).rev() {
                self.acc = (self.acc << 1) | ((v >> i) & 1) as u64;
                self.n += 1;
                if self.n == 8 {
                    self.out.push(self.acc as u8);
                    self.acc = 0;
                    self.n = 0;
                }
            }
        }
        fn finish(mut self) -> Vec<u8> {
            if self.n > 0 {
                self.out.push((self.acc << (8 - self.n)) as u8);
            }
            self.out.extend_from_slice(&[0; 16]);
            self.out
        }
    }

    // Code: diff lengths 0..=2 -> "00", "01", "10"; lengths 3..=6 -> "11" + 2 bits.
    fn table() -> Vec<(u8, u8)> {
        vec![
            (2, 0b0000_0000),
            (2, 0b0100_0000),
            (2, 0b1000_0000),
            (4, 0b1100_0000),
            (4, 0b1101_0000),
            (4, 0b1110_0000),
            (4, 0b1111_0000),
            (0, 0),
        ]
    }

    fn encode_diff(w: &mut W, d: i32) {
        let len = if d == 0 { 0 } else { 32 - (d.unsigned_abs()).leading_zeros() };
        let (code, clen) = [(0, 2), (1, 2), (2, 2), (12, 4), (13, 4), (14, 4), (15, 4)][len as usize];
        w.put(code, clen);
        if len > 0 {
            let v = if d > 0 { d as u32 } else { (d + (1 << len) - 1) as u32 };
            w.put(v, len);
        }
    }

    #[test]
    fn true_plane_roundtrip() {
        let (cols, rows, seed) = (7usize, 5usize, 512u16);
        let img: Vec<i32> = (0..cols * rows).map(|i| 500 + ((i * 7919) % 23) as i32 - 11).collect();
        let mut w = W::new();
        let mut row_start = [[seed as i32; 2]; 2];
        for r in 0..rows {
            let mut acc = [0i32; 2];
            for c in 0..cols {
                let v = img[r * cols + c];
                let prev = if c < 2 { row_start[r & 1][c & 1] } else { acc[c & 1] };
                encode_diff(&mut w, v - prev);
                acc[c & 1] = v;
                if c < 2 {
                    row_start[r & 1][c & 1] = v;
                }
            }
        }
        let data = w.finish();
        let hdr = TrueHeader {
            planes: None,
            seeds: [seed; 3],
            table: table(),
            plane_sizes: [data.len() as u32, 0, 0],
            data_offset: 0,
        };
        let code = hdr.code().unwrap();
        let mut out = vec![0u16; (cols - 1) * rows];
        decode_true_plane(&code, &data, seed, cols, rows, cols - 1, &mut out).unwrap();
        for r in 0..rows {
            for c in 0..cols - 1 {
                assert_eq!(out[r * (cols - 1) + c] as i32, img[r * cols + c]);
            }
        }
    }

    #[test]
    fn true_plane_truncated_is_error() {
        let code = TrueHeader { planes: None, seeds: [0; 3], table: table(), plane_sizes: [0; 3], data_offset: 0 }
            .code()
            .unwrap();
        let data = [0xFFu8; 4];
        let mut out = vec![0u16; 64 * 64];
        assert!(decode_true_plane(&code, &data, 0, 64, 64, 64, &mut out).is_err());
    }

    #[test]
    fn true_header_parse_and_ranges() {
        let mut d = Vec::new();
        for v in [512u16, 512, 512, 0] {
            d.extend_from_slice(&v.to_le_bytes());
        }
        for (l, c) in table() {
            d.extend_from_slice(&[l, c]);
        }
        for s in [17u32, 3, 40] {
            d.extend_from_slice(&s.to_le_bytes());
        }
        let h = TrueHeader::parse(&d, false).unwrap();
        assert_eq!(h.seeds, [512; 3]);
        assert_eq!(h.table.len(), 8);
        assert_eq!(h.data_offset, d.len());
        assert_eq!(h.plane_ranges(100).unwrap(), [(0, 100), (32, 100), (48, 100)]);
        assert!(h.plane_ranges(80).is_err());
        assert!(TrueHeader::parse(&d[..9], false).is_err());
    }

    #[test]
    fn prefix_code_long_codes() {
        // Codes longer than the LUT: 14 ones then 0/1.
        let codes = vec![(0, 1, 5), ((1 << 14) - 2, 14, 6), ((1 << 15) - 2, 15, 7), ((1 << 15) - 1, 15, 8)];
        let pc = PrefixCode::new(&codes).unwrap();
        let mut w = W::new();
        for &(c, l, _) in &codes {
            w.put(c, l);
        }
        let data = w.finish();
        let mut b = BitPumpMsb::new(&data);
        for &(_, _, v) in &codes {
            assert_eq!(pc.decode(&mut b).unwrap(), v);
        }
        assert!(PrefixCode::new(&[]).is_err());
        assert!(PrefixCode::new(&[(0, 0, 1)]).is_err());
    }

    #[test]
    fn huffman_offset_and_clamp() {
        // Symbols: "0" -> +5, "10" -> -7 (0xFFF9), "11" -> +1.
        let table: Vec<u32> = vec![(1 << 27), (2 << 27) | 2, (2 << 27) | 3];
        let mapping = vec![5u16, 0xFFF9, 1];
        let code = huffman_code(&table, &mapping).unwrap();
        // Row 0: R:-7 G:+5 B:+1 ; Row 1: R:+5 G:+5 B:+5.
        let mut w = W::new();
        let r0 = w.out.len();
        w.put(0b10, 2);
        w.put(0, 1);
        w.put(0b11, 2);
        w.put(0, 3); // pad to byte boundary
        let data0 = std::mem::take(&mut w.out);
        let mut w = W::new();
        w.put(0, 3);
        let mut data = data0.clone();
        data.extend(w.finish());
        let offs = [r0 as u32, data0.len() as u32];
        let mut out = vec![0u16; 6];
        decode_huffman(&code, &data, &offs, 1, 2, &mut out, false).unwrap();
        // min = -7 -> offset 7.
        assert_eq!(out, vec![0, 12, 8, 12, 12, 12]);
    }

    #[test]
    fn simple_decode() {
        let mapping: Vec<u16> = (0..1024).map(|i| i as u16).collect();
        let px = |r: u32, g: u32, b: u32| (r | (g << 10) | (b << 20)).to_le_bytes();
        let mut data = Vec::new();
        data.extend_from_slice(&px(10, 20, 30));
        data.extend_from_slice(&px(1, 2, 3));
        let mut out = vec![0u16; 6];
        decode_simple(&data, 8, &mapping, 10, 2, 1, &mut out, false).unwrap();
        assert_eq!(out, vec![10, 20, 30, 11, 22, 33]);
        assert!(decode_simple(&data[..6], 8, &mapping, 10, 2, 1, &mut out, false).is_err());
    }
}
