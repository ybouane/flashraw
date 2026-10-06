//! 12-bit DCT tiles used by early Blackmagic cinema DNGs (compression 7 with an
//! SOF1 frame instead of SOF3).
//!
//! Single-component sequential DCT: the DC coefficient is DPCM-coded with the class-0
//! Huffman table, AC coefficients use the class-1 table with JPEG run/size symbols,
//! quantisation tables carry 16-bit entries, and the inverse transform is the direct
//! separable cosine sum. The arithmetic below (f32 accumulation order, √½ scaling of
//! the first row/column, +0.5 then truncation, clamp to u16) is fixed so that the
//! output is bit-exact and deterministic across platforms.

use flashraw_core::bits::{BitPumpJpeg, BitRead, extend_diff};
use flashraw_core::huffman::HuffTable;
use flashraw_core::{Error, ReadAt, Result};

/// Zigzag order, padded so that run-lengths overshooting the block clamp to the last
/// coefficient instead of reading out of bounds.
const ZIGZAG: [u8; 80] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20, 13, 6, 7, 14, 21,
    28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54,
    47, 55, 62, 63, 63, 63, 63, 63, 63, 63, 63, 63, 63, 63, 63, 63, 63, 63, 63, 63,
];

/// Parsed stream headers.
pub struct Dct12 {
    pub width: u32,
    pub height: u32,
    pub precision: u32,
    quant: [i32; 64],
    dc: HuffTable,
    ac: HuffTable,
    scan_offset: usize,
}

impl Dct12 {
    pub fn parse(data: &[u8], dng_bug16: bool) -> Result<Self> {
        if data.len() < 4 || data[0] != 0xFF || data[1] != 0xD8 {
            return Err(Error::corrupt("DCT tile: missing SOI"));
        }
        let mut pos = 2usize;
        let mut quant = [0i32; 64];
        let mut dc: Option<HuffTable> = None;
        let mut ac: Option<HuffTable> = None;
        let mut frame = None;
        loop {
            while pos < data.len() && data[pos] != 0xFF {
                pos += 1;
            }
            while pos < data.len() && data[pos] == 0xFF {
                pos += 1;
            }
            let marker = *data.get(pos).ok_or_else(|| Error::corrupt("DCT tile: truncated header"))?;
            pos += 1;
            if marker == 0xD8 || marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
                continue;
            }
            let len = data.u16be(pos)? as usize;
            if len < 2 {
                return Err(Error::corrupt("DCT tile: invalid segment length"));
            }
            let seg = flashraw_core::bytes::slice_at(data, pos + 2, len - 2)?;
            match marker {
                0xDB => {
                    // 16-bit entries after the Pq/Tq byte (as written by these encoders).
                    for (i, q) in quant.iter_mut().enumerate() {
                        *q = seg.u16be(1 + 2 * i).unwrap_or(0) as i32;
                    }
                }
                0xC4 => {
                    let mut p = 0usize;
                    while p + 17 <= seg.len() {
                        let class = seg[p] >> 4;
                        let counts = &seg[p + 1..p + 17];
                        let total: usize = counts.iter().map(|&c| c as usize).sum();
                        let vals = flashraw_core::bytes::slice_at(seg, p + 17, total)?;
                        if class == 0 {
                            let mut t = HuffTable::from_counts(counts, vals, true)?;
                            t.dng_bug16 = dng_bug16;
                            dc.get_or_insert(t);
                        } else {
                            ac.get_or_insert(HuffTable::from_counts(counts, vals, false)?);
                        }
                        p += 17 + total;
                    }
                }
                0xC0 | 0xC1 => {
                    if seg.len() < 6 {
                        return Err(Error::corrupt("DCT tile: short SOF"));
                    }
                    if seg[5] != 1 {
                        return Err(Error::unsupported_compression("DNG", format!("{}-component DCT tile", seg[5])));
                    }
                    frame = Some((seg[0] as u32, seg.u16be(3)? as u32, seg.u16be(1)? as u32));
                }
                0xDA => {
                    let (precision, width, height) = frame.ok_or_else(|| Error::corrupt("DCT tile: SOS before SOF"))?;
                    let dc = dc.ok_or_else(|| Error::corrupt("DCT tile: missing DC table"))?;
                    let ac = ac.ok_or_else(|| Error::corrupt("DCT tile: missing AC table"))?;
                    return Ok(Dct12 { width, height, precision, quant, dc, ac, scan_offset: pos + len });
                }
                _ => {}
            }
            pos += len;
        }
    }

    /// Decode all 8x8 blocks in raster order, calling `sink(block_row, block_col, &block)`
    /// with `block_row/col` in pixel units (multiples of 8).
    pub fn decode_blocks<F: FnMut(u32, u32, &[u16; 64])>(&self, data: &[u8], mut sink: F) -> Result<()> {
        let scan = data.get(self.scan_offset..).ok_or_else(|| Error::corrupt("DCT tile: scan beyond data"))?;
        let mut bits = BitPumpJpeg::new(scan);
        let cs = cos_table();
        let mut dc_pred: i32 = 1 << 14;
        let mut out = [0u16; 64];
        let mut jrow = 0;
        while jrow + 7 < self.height {
            let mut jcol = 0;
            while jcol + 7 < self.width {
                let mut work = [[[0f32; 8]; 8]; 3];
                let diff = self.dc.decode_diff(&mut bits)?;
                dc_pred = dc_pred.wrapping_add(diff.wrapping_mul(self.quant[0]));
                work[0][0][0] = dc_pred as f32;
                let mut i = 1usize;
                while i < 64 {
                    let sym = self.ac.decode_symbol(&mut bits)?;
                    let skip = (sym >> 4) as usize;
                    let len = sym & 15;
                    i += skip;
                    if len == 0 && skip < 15 {
                        break;
                    }
                    let coef = if len == 0 { 0 } else { extend_diff(bits.get(len), len) };
                    let z = ZIGZAG[i.min(79)] as usize;
                    work[0][z / 8][z % 8] = (coef * self.quant[i.min(63)]) as f32;
                    i += 1;
                }
                let s = std::f64::consts::FRAC_1_SQRT_2 as f32;
                for c in 0..8 {
                    work[0][0][c] *= s;
                }
                for c in 0..8 {
                    work[0][c][0] *= s;
                }
                for i in 0..8 {
                    for j in 0..8 {
                        let mut acc = work[1][i][j];
                        for c in 0..8 {
                            acc += work[0][i][c] * cs[(j * 2 + 1) * c];
                        }
                        work[1][i][j] = acc;
                    }
                }
                for i in 0..8 {
                    for j in 0..8 {
                        let mut acc = work[2][i][j];
                        for c in 0..8 {
                            acc += work[1][c][j] * cs[(i * 2 + 1) * c];
                        }
                        work[2][i][j] = acc;
                    }
                }
                for (k, o) in out.iter_mut().enumerate() {
                    let v = (work[2][k / 8][k % 8] as f64 + 0.5) as i64;
                    *o = v.clamp(0, 65535) as u16;
                }
                if bits.overrun() > 64 {
                    return Err(Error::corrupt("DCT tile: data ends early"));
                }
                sink(jrow, jcol, &out);
                jcol += 8;
            }
            jrow += 8;
        }
        Ok(())
    }
}

fn cos_table() -> &'static [f32; 106] {
    use std::sync::OnceLock;
    static T: OnceLock<[f32; 106]> = OnceLock::new();
    T.get_or_init(|| {
        let mut t = [0f32; 106];
        for (c, v) in t.iter_mut().enumerate() {
            *v = ((((c & 31) as f64) * std::f64::consts::PI / 16.0).cos() / 2.0) as f32;
        }
        t
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_table() {
        let t = cos_table();
        assert!((t[0] - 0.5).abs() < 1e-7);
        assert!((t[8]).abs() < 1e-7);
        assert!((t[16] + 0.5).abs() < 1e-7);
    }

    #[test]
    fn rejects_garbage() {
        assert!(Dct12::parse(&[0, 1, 2, 3], true).is_err());
        assert!(Dct12::parse(&[0xFF, 0xD8, 0xFF, 0xDA, 0, 2], true).is_err());
    }
}
