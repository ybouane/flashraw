//! Sony "lossless compressed" S/M sizes: YCbCr lossless JPEG tiles.
//!
//! Each tile is a 3-component SOF3 stream with luma subsampling 2x2 (4:2:0) or 2x1
//! (4:2:2). An MCU holds 4 (or 2) luma samples and one Cb/Cr pair. Prediction is
//! Sony-specific rather than the JPEG predictor: within an MCU luma samples chain
//! (Y2 from Y1, Y4 from Y3, Y3 from the left MCU's Y4 or, in the first column,
//! from Y1), MCUs predict from the MCU to the left, and the first MCU of a row pair
//! predicts from the first pixel of the row(s) above. The tile is reconstructed as
//! full-resolution YCbCr (chroma replicated), then converted to RGB with the
//! standard full-range JFIF equations around a chroma offset of 16383.

use crate::ljpeg::{LjpegHeader, LjpegOptions};
use flashraw_core::bits::{BitPumpJpeg, BitRead};
use flashraw_core::huffman::HuffTable;
use flashraw_core::{Error, Result};

/// Decode one tile into `tile_w * tile_h * 3` interleaved `[Y, Cb, Cr]` samples.
pub fn decode_tile(src: &[u8], tile_w: usize, tile_h: usize) -> Result<Vec<u16>> {
    let hdr = LjpegHeader::parse(src, LjpegOptions::default())?;
    if hdr.components.len() != 3 {
        return Err(Error::corrupt(format!("Sony YCbCr tile with {} components", hdr.components.len())));
    }
    if hdr.width as usize != tile_w || hdr.height as usize != tile_h || tile_w % 2 != 0 || tile_h % 2 != 0 {
        return Err(Error::corrupt(format!(
            "Sony YCbCr tile is {}x{}, expected {tile_w}x{tile_h}",
            hdr.width, hdr.height
        )));
    }
    let (h, v) = (hdr.components[0].h, hdr.components[0].v);
    let t = |i: usize| hdr.table(i).ok_or_else(|| Error::corrupt("Sony YCbCr: missing Huffman table"));
    let scan = src.get(hdr.scan_offset..).ok_or_else(|| Error::corrupt("Sony YCbCr: scan beyond data"))?;
    let mut bits = BitPumpJpeg::new(scan);
    let mut out = vec![0u16; tile_w * tile_h * 3];
    let base = 1i32 << (hdr.bits - 1);
    match (h, v) {
        (2, 2) => decode_420(&mut bits, t(0)?, t(4)?, t(5)?, &mut out, tile_w * 3, tile_h, base)?,
        (2, 1) => decode_422(&mut bits, t(0)?, t(2)?, t(3)?, &mut out, tile_w * 3, tile_h, base)?,
        _ => return Err(Error::unsupported_compression("ARW", format!("YCbCr sampling {h}x{v}"))),
    }
    if bits.overrun() > 64 {
        return Err(Error::corrupt("Sony YCbCr tile truncated"));
    }
    Ok(out)
}

#[inline(always)]
fn put(out: &mut [u16], pix: usize, y: i32, cb: i32, cr: i32) {
    out[pix] = y as u16;
    out[pix + 1] = cb as u16;
    out[pix + 2] = cr as u16;
}

/// 4:2:0. `width` is in samples (3 per pixel); MCUs advance 6 samples.
#[allow(clippy::too_many_arguments)]
fn decode_420(
    bits: &mut BitPumpJpeg,
    hy: &HuffTable,
    hcb: &HuffTable,
    hcr: &HuffTable,
    out: &mut [u16],
    width: usize,
    height: usize,
    base: i32,
) -> Result<()> {
    let mcu = |out: &mut [u16],
               bits: &mut BitPumpJpeg,
               row: usize,
               col: usize,
               py1: i32,
               py3: Option<i32>,
               pcb: i32,
               pcr: i32|
     -> Result<()> {
        let y1 = py1 + hy.decode_diff(bits)?;
        let y2 = y1 + hy.decode_diff(bits)?;
        let y3 = py3.unwrap_or(y1) + hy.decode_diff(bits)?;
        let y4 = y3 + hy.decode_diff(bits)?;
        let cb = pcb + hcb.decode_diff(bits)?;
        let cr = pcr + hcr.decode_diff(bits)?;
        let p1 = row * width + col;
        let p3 = (row + 1) * width + col;
        put(out, p1, y1, cb, cr);
        put(out, p1 + 3, y2, cb, cr);
        put(out, p3, y3, cb, cr);
        put(out, p3 + 3, y4, cb, cr);
        Ok(())
    };
    mcu(out, bits, 0, 0, base, None, base, base)?;
    for row in (0..height).step_by(2) {
        let start = if row == 0 { 6 } else { 0 };
        for col in (start..width).step_by(6) {
            if col == 0 {
                let pos = (row - 2) * width;
                let (py1, pcb, pcr) = (out[pos] as i32, out[pos + 1] as i32, out[pos + 2] as i32);
                mcu(out, bits, row, col, py1, None, pcb, pcr)?;
            } else {
                let p1 = row * width + col - 3;
                let p3 = (row + 1) * width + col - 3;
                let (py1, py3, pcb, pcr) = (out[p1] as i32, out[p3] as i32, out[p1 + 1] as i32, out[p1 + 2] as i32);
                mcu(out, bits, row, col, py1, Some(py3), pcb, pcr)?;
            }
        }
        if bits.overrun() > 64 {
            return Err(Error::corrupt("Sony YCbCr tile truncated"));
        }
    }
    Ok(())
}

/// 4:2:2. `width` is in samples (3 per pixel); MCUs advance 6 samples.
#[allow(clippy::too_many_arguments)]
fn decode_422(
    bits: &mut BitPumpJpeg,
    hy: &HuffTable,
    hcb: &HuffTable,
    hcr: &HuffTable,
    out: &mut [u16],
    width: usize,
    height: usize,
    base: i32,
) -> Result<()> {
    let mcu =
        |out: &mut [u16], bits: &mut BitPumpJpeg, row: usize, col: usize, py: i32, pcb: i32, pcr: i32| -> Result<()> {
            let y1 = py + hy.decode_diff(bits)?;
            let y2 = y1 + hy.decode_diff(bits)?;
            let cb = pcb + hcb.decode_diff(bits)?;
            let cr = pcr + hcr.decode_diff(bits)?;
            let p = row * width + col;
            put(out, p, y1, cb, cr);
            put(out, p + 3, y2, cb, cr);
            Ok(())
        };
    mcu(out, bits, 0, 0, base, base, base)?;
    for row in 0..height {
        let start = if row == 0 { 6 } else { 0 };
        for col in (start..width).step_by(6) {
            let pos = if col == 0 { (row - 1) * width } else { row * width + col - 3 };
            let (py, pcb, pcr) = (out[pos] as i32, out[pos + 1] as i32, out[pos + 2] as i32);
            mcu(out, bits, row, col, py, pcb, pcr)?;
        }
        if bits.overrun() > 64 {
            return Err(Error::corrupt("Sony YCbCr tile truncated"));
        }
    }
    Ok(())
}

/// Convert one YCbCr sample (chroma centred on 16383) to clamped 16-bit RGB, using
#[inline]
pub fn to_rgb(y: u16, cb: u16, cr: u16) -> [u16; 3] {
    const CDELTA: i32 = 16383;
    let y = y as f32;
    let cb = (cb as i32 - CDELTA) as f32;
    let cr = (cr as i32 - CDELTA) as f32;
    let r = y + 1.40200f32 * cr;
    let g = y - 0.34414f32 * cb - 0.71414f32 * cr;
    let b = y + 1.77200f32 * cb;
    [lim16(r), lim16(g), lim16(b)]
}

#[inline(always)]
fn lim16(q: f32) -> u16 {
    if q < 0.0 {
        0
    } else if q > 65535.0 {
        65535
    } else {
        q as u32 as u16
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Bw {
        out: Vec<u8>,
        acc: u32,
        n: u32,
    }
    impl Bw {
        fn put(&mut self, v: u32, len: u32) {
            for i in (0..len).rev() {
                self.acc = self.acc << 1 | ((v >> i) & 1);
                self.n += 1;
                if self.n == 8 {
                    self.out.push(self.acc as u8);
                    if self.acc as u8 == 0xFF {
                        self.out.push(0);
                    }
                    self.acc = 0;
                    self.n = 0;
                }
            }
        }
        fn diff(&mut self, d: i32) {
            // Table: symbol s has code (s) in 5 bits (17 symbols < 32).
            let len = if d == 0 { 0 } else { 32 - d.unsigned_abs().leading_zeros() };
            self.put(len, 5);
            if len > 0 {
                let v = if d < 0 { (d - 1) as u32 & ((1 << len) - 1) } else { d as u32 };
                self.put(v, len);
            }
        }
    }

    /// Build a 3-component SOF3 stream with luma sampling h x v and a flat 5-bit code table.
    fn stream(w: usize, h: usize, hs: u8, vs: u8, body: &[u8]) -> Vec<u8> {
        let mut o = vec![0xFF, 0xD8, 0xFF, 0xC3, 0, 17, 14];
        o.extend((h as u16).to_be_bytes());
        o.extend((w as u16).to_be_bytes());
        o.extend([3, 1, hs << 4 | vs, 0, 2, 0x11, 0, 3, 0x11, 0]);
        // DHT: 17 symbols of length 5.
        let mut counts = [0u8; 16];
        counts[4] = 17;
        o.extend([0xFF, 0xC4]);
        o.extend(((2 + 17 + 17) as u16).to_be_bytes());
        o.push(0);
        o.extend(counts);
        o.extend(0u8..17);
        o.extend([0xFF, 0xDA, 0, 12, 3, 1, 0, 2, 0, 3, 0, 1, 0, 0]);
        o.extend_from_slice(body);
        o.extend([0xFF, 0xD9]);
        o
    }

    #[test]
    fn decode_420_matches_reference_prediction() {
        let (w, h) = (8usize, 4usize);
        // Target image (Y per pixel, chroma per 2x2 block).
        let y = |r: usize, c: usize| (1000 + r * 37 + c * 11) as i32;
        let cb = |r: usize, c: usize| (16000 + (r / 2) * 5 + (c / 2) * 3) as i32;
        let cr = |r: usize, c: usize| (16500 - (r / 2) * 7 - (c / 2) * 2) as i32;
        let base = 1 << 13; // 14-bit precision
        let mut bw = Bw { out: vec![], acc: 0, n: 0 };
        for row in (0..h).step_by(2) {
            for col in (0..w).step_by(2) {
                let (py1, py3, pcb, pcr) = if row == 0 && col == 0 {
                    (base, None, base, base)
                } else if col == 0 {
                    (y(row - 2, 0), None, cb(row - 2, 0), cr(row - 2, 0))
                } else {
                    (y(row, col - 1), Some(y(row + 1, col - 1)), cb(row, col - 1), cr(row, col - 1))
                };
                bw.diff(y(row, col) - py1);
                bw.diff(y(row, col + 1) - y(row, col));
                bw.diff(y(row + 1, col) - py3.unwrap_or(y(row, col)));
                bw.diff(y(row + 1, col + 1) - y(row + 1, col));
                bw.diff(cb(row, col) - pcb);
                bw.diff(cr(row, col) - pcr);
            }
        }
        bw.put(0x7F, 7);
        let s = stream(w, h, 2, 2, &bw.out);
        let out = decode_tile(&s, w, h).unwrap();
        for r in 0..h {
            for c in 0..w {
                let p = (r * w + c) * 3;
                assert_eq!(out[p] as i32, y(r, c), "Y at {r},{c}");
                assert_eq!(out[p + 1] as i32, cb(r, c));
                assert_eq!(out[p + 2] as i32, cr(r, c));
            }
        }
        // Truncation is detected.
        assert!(decode_tile(&s[..s.len() / 2], w, h).is_err());
    }

    #[test]
    fn rgb_conversion_clamps() {
        assert_eq!(to_rgb(1000, 16383, 16383), [1000, 1000, 1000]);
        assert_eq!(to_rgb(0, 0, 0)[0], 0);
        assert_eq!(to_rgb(65535, 65535, 65535)[2], 65535);
    }
}
