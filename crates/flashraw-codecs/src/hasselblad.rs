//! Hasselblad lossless JPEG variant (3FR/FFF, Imacon/Flexcolor, H/X/CFV backs).
//!
//! The stream starts with ordinary JPEG markers (SOF3, DHT, SOS) but the scan is not
//! standard lossless JPEG:
//!
//! - bits are read MSB-first from 32-bit **little-endian** words, with no `FF 00`
//!   byte stuffing;
//! - pixels are coded in pairs: two Huffman length symbols, then the two difference
//!   values (a length-16 code of all ones means -32768);
//! - prediction is from the same-colour neighbour two columns to the left, starting
//!   each row at `0x8000 + base_offset`; predictor 11 additionally adds half the
//!   horizontal gradient of the row two above.
//!
//! The stream is a single sequential bit stream (no restart markers or row index).

use flashraw_core::bits::{BitPumpMsb32, BitRead};
use flashraw_core::huffman::HuffTable;
use flashraw_core::{Error, ReadAt, Result};

/// Parsed header of a Hasselblad stream.
#[derive(Debug, Clone)]
pub struct HasselbladHeader {
    pub table: HuffTable,
    /// Predictor selector from the SOS header (1 or 11 in practice).
    pub predictor: u32,
    /// Frame dimensions as declared by SOF3 (informational; the container decides).
    pub width: u32,
    pub height: u32,
    /// Offset of the scan data.
    pub scan_offset: usize,
}

impl HasselbladHeader {
    /// Parse markers up to the start of the scan.
    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < 4 || data[0] != 0xFF || data[1] != 0xD8 {
            return Err(Error::corrupt("Hasselblad: missing SOI"));
        }
        let mut pos = 2usize;
        let mut table: Option<HuffTable> = None;
        let (mut width, mut height) = (0u32, 0u32);
        loop {
            while pos < data.len() && data[pos] != 0xFF {
                pos += 1;
            }
            while pos < data.len() && data[pos] == 0xFF {
                pos += 1;
            }
            let marker = *data.get(pos).ok_or_else(|| Error::corrupt("Hasselblad: truncated header"))?;
            pos += 1;
            if marker == 0xD8 || (0xD0..=0xD7).contains(&marker) {
                continue;
            }
            if marker == 0xD9 {
                return Err(Error::corrupt("Hasselblad: EOI before SOS"));
            }
            let len = data.u16be(pos)? as usize;
            if len < 2 {
                return Err(Error::corrupt("Hasselblad: bad segment length"));
            }
            let seg = flashraw_core::bytes::slice_at(data, pos + 2, len - 2)?;
            match marker {
                0xC3 => {
                    if seg.len() >= 5 {
                        height = seg.u16be(1)? as u32;
                        width = seg.u16be(3)? as u32;
                    }
                }
                0xC4 => {
                    let mut p = 0usize;
                    while p + 17 <= seg.len() {
                        let id = seg[p] & 0x0F;
                        let counts = &seg[p + 1..p + 17];
                        let total: usize = counts.iter().map(|&c| c as usize).sum();
                        let vals = flashraw_core::bytes::slice_at(seg, p + 17, total)?;
                        if id == 0 || table.is_none() {
                            table = Some(HuffTable::from_counts(counts, vals, false)?);
                        }
                        p += 17 + total;
                    }
                }
                0xDA => {
                    let ns = *seg.first().ok_or_else(|| Error::corrupt("Hasselblad: short SOS"))? as usize;
                    let predictor = *seg.get(1 + 2 * ns).ok_or_else(|| Error::corrupt("Hasselblad: short SOS"))? as u32;
                    let table = table.ok_or_else(|| Error::corrupt("Hasselblad: missing Huffman table"))?;
                    return Ok(HasselbladHeader { table, predictor, width, height, scan_offset: pos + len });
                }
                _ => {}
            }
            pos += len;
        }
    }
}

/// Read a difference of `len` bits (JPEG-style sign extension, with the
/// Hasselblad all-ones length-16 escape).
#[inline(always)]
fn diff<B: BitRead>(bits: &mut B, len: u32) -> i32 {
    if len == 0 {
        return 0;
    }
    let mut d = bits.get(len) as i32;
    if d & (1 << (len - 1)) == 0 {
        d -= (1 << len) - 1;
    }
    if d == 65535 { -32768 } else { d }
}

/// Decode a single-shot stream into `out` (`width x height`, row-major).
/// `base` is added to the row-start predictor (0 except for some H4D-60 3FR files).
pub fn decode(
    data: &[u8],
    hdr: &HasselbladHeader,
    out: &mut [u16],
    width: usize,
    height: usize,
    base: i32,
) -> Result<()> {
    if width % 2 != 0 {
        return Err(Error::unsupported_compression("Hasselblad", "odd raster width"));
    }
    if out.len() < width * height {
        return Err(Error::InvalidArgument("Hasselblad: output buffer too small".into()));
    }
    let scan = data.get(hdr.scan_offset..).ok_or_else(|| Error::corrupt("Hasselblad: scan beyond data"))?;
    let mut bits = BitPumpMsb32::new(scan);
    let t = &hdr.table;
    let grad = hdr.predictor == 11;
    // Predictor history: current row and the row two above (same CFA colours).
    let mut rows: [Vec<i32>; 3] = [vec![0; width], vec![0; width], vec![0; width]];
    let start = 0x8000 + base;
    for row in 0..height {
        rows.rotate_left(1); // rows[2] := buffer of row-3 (reused for the current row)
        let (prev2, rest) = rows.split_at_mut(1);
        let up2 = &prev2[0];
        let cur = &mut rest[1];
        let dst = &mut out[row * width..(row + 1) * width];
        for col in (0..width).step_by(2) {
            let l0 = t.decode_symbol(&mut bits)?;
            let l1 = t.decode_symbol(&mut bits)?;
            if l0 > 16 || l1 > 16 {
                return Err(Error::corrupt("Hasselblad: difference length > 16"));
            }
            let d = [diff(&mut bits, l0), diff(&mut bits, l1)];
            for (k, &dk) in d.iter().enumerate() {
                let s = col + k;
                let mut pred = if col > 0 { cur[s - 2] } else { start };
                if grad && col > 0 && row > 1 {
                    pred += up2[s] / 2 - up2[s - 2] / 2;
                }
                pred = pred.wrapping_add(dk);
                dst[s] = pred as u16;
                cur[s] = pred;
            }
        }
        if bits.exhausted() {
            return Err(Error::corrupt(format!("Hasselblad: data ends at row {row}/{height}")));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal encoder for predictor 1 (two lengths, two diffs per pixel pair).
    fn encode(width: usize, height: usize, samples: &[u16]) -> Vec<u8> {
        // Table: lengths 0..=16, codes of length 5 for all 17 symbols (fits in 32 codes).
        let counts: [u8; 16] = [0, 0, 0, 0, 17, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let values: Vec<u8> = (0..=16).collect();
        let mut out = vec![0xFF, 0xD8];
        out.extend([
            0xFF,
            0xC3,
            0,
            11,
            16,
            (height >> 8) as u8,
            height as u8,
            (width >> 8) as u8,
            width as u8,
            1,
            1,
            0x11,
            0,
        ]);
        out.extend([0xFF, 0xC4, 0, (2 + 17 + values.len()) as u8, 0x00]);
        out.extend(counts);
        out.extend(&values);
        out.extend([0xFF, 0xDA, 0, 8, 1, 1, 0, 1, 0, 0]);
        let mut words: Vec<u32> = vec![];
        let (mut acc, mut n) = (0u64, 0u32);
        let mut put = |v: u32, b: u32, words: &mut Vec<u32>| {
            if b == 0 {
                return;
            }
            acc = (acc << b) | (v as u64 & ((1u64 << b) - 1));
            n += b;
            while n >= 32 {
                words.push((acc >> (n - 32)) as u32);
                n -= 32;
            }
        };
        for row in 0..height {
            for col in (0..width).step_by(2) {
                let mut ds = [0i32; 2];
                for (k, dk) in ds.iter_mut().enumerate() {
                    let s = col + k;
                    let pred = if col > 0 { samples[row * width + s - 2] as i32 } else { 0x8000 };
                    *dk = samples[row * width + s] as i32 - pred;
                }
                let lens: Vec<u32> =
                    ds.iter().map(|&d| if d == 0 { 0 } else { 32 - (d.unsigned_abs()).leading_zeros() }).collect();
                for &l in &lens {
                    put(l, 5, &mut words); // symbol l has code l (5 bits)
                }
                for k in 0..2 {
                    let (d, l) = (ds[k], lens[k]);
                    let v = if d >= 0 { d as u32 } else { (d + (1 << l) - 1) as u32 };
                    put(v, l, &mut words);
                }
            }
        }
        put(0, 31, &mut words);
        put(0, 32, &mut words);
        for w in words {
            out.extend(w.to_le_bytes());
        }
        out
    }

    #[test]
    fn round_trip() {
        let (w, h) = (16usize, 5usize);
        let samples: Vec<u16> = (0..w * h).map(|i| ((i * 2654435761usize) >> 7) as u16 & 0x3FFF).collect();
        let data = encode(w, h, &samples);
        let hdr = HasselbladHeader::parse(&data).unwrap();
        assert_eq!((hdr.width, hdr.height, hdr.predictor), (w as u32, h as u32, 1));
        let mut out = vec![0u16; w * h];
        decode(&data, &hdr, &mut out, w, h, 0).unwrap();
        assert_eq!(out, samples);
    }

    #[test]
    fn truncated_and_garbage() {
        let (w, h) = (16usize, 40usize);
        let samples: Vec<u16> = (0..w * h).map(|i| (i * 977 % 60000) as u16).collect();
        let data = encode(w, h, &samples);
        let hdr = HasselbladHeader::parse(&data).unwrap();
        let mut out = vec![0u16; w * h];
        assert!(decode(&data[..hdr.scan_offset + 20], &hdr, &mut out, w, h, 0).is_err());
        for n in 0..hdr.scan_offset {
            let _ = HasselbladHeader::parse(&data[..n]);
        }
        let mut g = data.clone();
        for b in g[hdr.scan_offset..].iter_mut() {
            *b = b.wrapping_mul(31).wrapping_add(7);
        }
        let _ = decode(&g, &hdr, &mut out, w, h, 0);
    }
}
