//! Packings used by headerless sensor dumps (CHDK, phone/action-camera dumps,
//! Raspberry Pi, old compacts).
//!
//! - [`packed_rows`]: the "packed" layout family, parameterised by load flags: samples of `bits` bits read MSB-first from little-endian words of 8, 16,
//!   24 or 32 bits (`flags & 24`), rows padded to an even byte count (`flags & 128`),
//!   a pad byte after every 10 samples (`flags & 1`), sample pairs swapped
//!   (`flags & 64`) and two interlaced fields (`flags & 2`, second field located at
//!   half the file when `flags & 4`). Rows are independent whenever they start on a
//!   word boundary, which is the common case and decodes in parallel; every other
//!   combination runs through a sequential reference implementation.
//! - [`mipi10_rows`]: MIPI RAW10 (4 samples in 5 bytes, low bits in the 5th byte).
//! - [`loose10_rows`]: Android "loose" RAW10 (6 samples in each little-endian 64-bit word).
//! - [`u16_rows`]: 16-bit little/big-endian samples with an optional right shift.

use crate::packed::unpack_row;
use flashraw_core::{Error, PackOrder, Result, par};

/// Geometry of a "packed" payload.
#[derive(Debug, Clone, Copy)]
pub struct PackedParams {
    pub width: usize,
    pub height: usize,
    pub bits: u32,
    /// Load flags (see the module docs).
    pub flags: u32,
}

impl PackedParams {
    /// Bytes per row (before the 10-sample padding expansion).
    pub fn row_bytes(&self) -> usize {
        let mut b = self.width * self.bits as usize / 8;
        if self.flags & 128 != 0 {
            b += b & 1;
        }
        b
    }
    fn word_bits(&self) -> u32 {
        8 + (self.flags & 24)
    }
}

/// Decode a "packed" payload. `file` is the whole file (the interlaced layout
/// addresses its second field relative to the file size), `offset` the payload start.
/// Rows past the end of the data are zero-filled.
pub fn packed_rows(file: &[u8], offset: usize, p: PackedParams, out: &mut [u16], parallel: bool) -> Result<()> {
    let (w, h) = (p.width, p.height);
    if !(1..=16).contains(&p.bits) || w == 0 || out.len() < w * h {
        return Err(Error::corrupt("invalid packed raster geometry"));
    }
    let bwide = p.row_bytes();
    let word = p.word_bits() as usize;
    let fast_order = match word {
        8 => Some(PackOrder::MsbBytes),
        16 => Some(PackOrder::Msb16),
        32 => Some(PackOrder::Msb32),
        _ => None,
    };
    if let (Some(order), true) = (fast_order, p.flags & 3 == 0 && (bwide * 8) % word == 0) {
        let src = file.get(offset..).unwrap_or(&[]);
        let swap = p.flags & 64 != 0;
        par::for_each_row_chunk(&mut out[..w * h], w, 16, parallel, |first, chunk| {
            for (i, row) in chunk.chunks_exact_mut(w).enumerate() {
                let y = first + i;
                let Some(s) = src.get(y * bwide..) else { break };
                let s = &s[..bwide.min(s.len())];
                if s.len() * 8 < w * p.bits as usize {
                    // Truncated final row: decode through a zero-padded copy.
                    let mut tmp = s.to_vec();
                    tmp.resize(bwide + 8, 0);
                    unpack_row(&tmp, row, p.bits, order, false);
                } else {
                    unpack_row(s, row, p.bits, order, true);
                }
                if swap {
                    for pair in row.chunks_exact_mut(2) {
                        pair.swap(0, 1);
                    }
                }
            }
        });
        return Ok(());
    }
    packed_rows_reference(file, offset, p, out);
    Ok(())
}

/// Bit-exact sequential decode of any packed layout (reference implementation for the
/// parallel path).
pub fn packed_rows_reference(file: &[u8], offset: usize, p: PackedParams, out: &mut [u16]) {
    let (w, h, bits) = (p.width, p.height, p.bits as i64);
    let bwide = p.row_bytes();
    let rbits = bwide as i64 * 8 - w as i64 * bits;
    let bite = p.word_bits() as i64;
    let half = h.div_ceil(2);
    let mut pos = offset;
    let mut bitbuf: u64 = 0;
    let mut vbits: i64 = 0;
    let byte = |pos: &mut usize| -> u64 {
        let b = file.get(*pos).copied().unwrap_or(0);
        *pos += 1;
        b as u64
    };
    let swap = (p.flags >> 6 & 1) as usize;
    for irow in 0..h {
        let mut row = irow;
        if p.flags & 2 != 0 {
            row = irow % half * 2 + irow / half;
            if row == 1 && p.flags & 4 != 0 {
                vbits = 0;
                pos = file.len() >> 3 << 2;
            }
        }
        if pos > file.len() {
            break;
        }
        for col in 0..w {
            vbits -= bits;
            while vbits < 0 {
                bitbuf <<= bite as u32;
                for i in (0..bite).step_by(8) {
                    bitbuf |= byte(&mut pos) << i;
                }
                vbits += bite;
            }
            // 0 <= vbits < bite <= 32 and bits <= 16, so both shifts are in range.
            let val = ((bitbuf << (64 - bits - vbits) as u32) >> (64 - bits) as u32) & 0x7fff_ffff;
            if let Some(o) = out.get_mut(row * w + (col ^ swap)).filter(|_| (col ^ swap) < w) {
                *o = val as u16;
            }
            if p.flags & 1 != 0 && col % 10 == 9 {
                pos += 1;
            }
        }
        vbits -= rbits;
    }
}

/// MIPI RAW10: 4 samples in 5 bytes (8 high bits each, then a byte of 4x2 low bits).
/// `stride` is the byte distance between rows; with `reverse_words` each group of 4
/// stored bytes is byte-reversed first (Broadcom little-endian dumps).
pub fn mipi10_rows(src: &[u8], width: usize, height: usize, stride: usize, out: &mut [u16], parallel: bool) {
    let need = width.div_ceil(4) * 5;
    par::for_each_row_chunk(&mut out[..width * height], width, 32, parallel, |first, chunk| {
        let mut tmp = vec![0u8; need + 8];
        for (i, row) in chunk.chunks_exact_mut(width).enumerate() {
            let y = first + i;
            let Some(s) = src.get(y * stride..) else { break };
            let n = need.min(s.len());
            tmp[..n].copy_from_slice(&s[..n]);
            tmp[n..].fill(0);
            for (g, px) in row.chunks_mut(4).enumerate() {
                let d = &tmp[g * 5..g * 5 + 5];
                for (c, v) in px.iter_mut().enumerate() {
                    *v = ((d[c] as u16) << 2) | ((d[4] >> (c * 2)) & 3) as u16;
                }
            }
        }
    });
}

/// Android "loose" RAW10: each little-endian 64-bit word carries 6 samples (LSB first),
/// rows of `ceil(width / 6) * 8` bytes.
pub fn loose10_rows(src: &[u8], width: usize, height: usize, out: &mut [u16], parallel: bool) {
    let bwide = width.div_ceil(6) * 8;
    par::for_each_row_chunk(&mut out[..width * height], width, 32, parallel, |first, chunk| {
        for (i, row) in chunk.chunks_exact_mut(width).enumerate() {
            let y = first + i;
            let Some(s) = src.get(y * bwide..) else { break };
            for (g, px) in row.chunks_mut(6).enumerate() {
                let mut b = [0u8; 8];
                if let Some(word) = s.get(g * 8..g * 8 + 8) {
                    b.copy_from_slice(word);
                }
                let v = u64::from_le_bytes(b);
                for (c, o) in px.iter_mut().enumerate() {
                    *o = (v >> (c * 10) & 0x3ff) as u16;
                }
            }
        }
    });
}

/// 16-bit samples (little- or big-endian), shifted right by `shift`, rows of `width`
/// samples; `bottom_up` stores the last row first.
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
    let rb = width * 2;
    par::for_each_row_chunk(&mut out[..width * height], width, 32, parallel, |first, chunk| {
        for (i, row) in chunk.chunks_exact_mut(width).enumerate() {
            let y = first + i;
            let sy = if bottom_up { height - 1 - y } else { y };
            let Some(s) = src.get(sy * rb..) else { continue };
            for (o, b) in row.iter_mut().zip(s.chunks_exact(2)) {
                let v = if big_endian { u16::from_be_bytes([b[0], b[1]]) } else { u16::from_le_bytes([b[0], b[1]]) };
                *o = v >> shift.min(15);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lcg(n: usize, seed: u32) -> Vec<u8> {
        let mut s = seed;
        (0..n)
            .map(|_| {
                s = s.wrapping_mul(1664525).wrapping_add(1013904223);
                (s >> 24) as u8
            })
            .collect()
    }

    #[test]
    fn fast_path_matches_reference() {
        for &(w, h, bits, flags) in &[
            (3152usize, 5usize, 10u32, 40u32 | 128),
            (2346, 4, 12, 128),
            (4320, 3, 10, 224),
            (100, 6, 12, 24 | 128),
            (64, 4, 16, 0),
        ] {
            let p = PackedParams { width: w, height: h, bits, flags };
            let file = lcg(16 + p.row_bytes() * h + 16, w as u32);
            let mut a = vec![0u16; w * h];
            let mut b = vec![0u16; w * h];
            packed_rows(&file, 16, p, &mut a, true).unwrap();
            packed_rows_reference(&file, 16, p, &mut b);
            assert_eq!(a, b, "{w}x{h} {bits} bits flags {flags}");
        }
    }

    #[test]
    fn reference_handles_odd_flags_without_panicking() {
        for flags in [1u32, 2, 6, 7, 9, 16, 65, 255] {
            let p = PackedParams { width: 31, height: 7, bits: 12, flags };
            let file = lcg(400, flags);
            let mut out = vec![0u16; 31 * 7];
            packed_rows(&file, 3, p, &mut out, false).unwrap();
            // Truncated input must not panic either.
            packed_rows(&file[..20], 3, p, &mut out, false).unwrap();
        }
    }

    #[test]
    fn mipi_and_loose() {
        let src = [0x12u8, 0x34, 0x56, 0x78, 0b11_10_01_00];
        let mut out = vec![0u16; 4];
        mipi10_rows(&src, 4, 1, 5, &mut out, false);
        assert_eq!(out, vec![0x12 << 2, (0x34 << 2) | 1, (0x56 << 2) | 2, (0x78 << 2) | 3]);
        let v: u64 = (1..=6).fold(0, |acc, i| acc | ((i as u64 * 100) << ((i - 1) * 10)));
        let mut out = vec![0u16; 6];
        loose10_rows(&v.to_le_bytes(), 6, 1, &mut out, false);
        assert_eq!(out, vec![100, 200, 300, 400, 500, 600]);
    }

    #[test]
    fn u16_variants() {
        let src = [0x01u8, 0x02, 0x03, 0x04];
        let mut out = vec![0u16; 2];
        u16_rows(&src, 2, 1, false, 0, false, &mut out, false);
        assert_eq!(out, vec![0x0201, 0x0403]);
        u16_rows(&src, 2, 1, true, 4, false, &mut out, false);
        assert_eq!(out, vec![0x0102 >> 4, 0x0304 >> 4]);
        let mut out = vec![0u16; 2];
        u16_rows(&src, 1, 2, false, 0, true, &mut out, false);
        assert_eq!(out, vec![0x0403, 0x0201]);
    }
}
