//! Sony "lossless compressed" S and M sizes: YCbCr lossless JPEG tiles.
//!
//! Each tile is a three-component lossless JPEG (SOF3) whose luma is sampled 2x2 (4:2:0, medium size)
//! or 2x1 (4:2:2, small size). Samples are predicted from the left neighbour (or, at the start of a
//! row, from the first pixel of the previous MCU row); the first sample of a tile starts from
//! `2^(precision - 1)`. Chroma is centred on 16383.

use crate::ljpeg::{LjpegHeader, LjpegOptions};
use flashraw_core::bits::{BitPumpJpeg, BitRead};
use flashraw_core::huffman::HuffTable;
use flashraw_core::{Error, Result};

/// Weights of the colour differences in the YCbCr to RGB conversion (full-range JFIF).
const R_FROM_CR: f32 = 1.402;
const G_FROM_CB: f32 = 0.34414;
const G_FROM_CR: f32 = 0.71414;
const B_FROM_CB: f32 = 1.772;
/// Chroma samples are stored centred on this value.
const CHROMA_CENTRE: i32 = 16383;

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
    if hdr.scan_offset > src.len() {
        return Err(Error::corrupt("Sony YCbCr: scan beyond data"));
    }
    let (lh, lv) = (hdr.components[0].h, hdr.components[0].v);
    let n_pos = match (lh, lv) {
        (2, 2) => 6,
        (2, 1) => 4,
        _ => return Err(Error::unsupported_compression("ARW", format!("YCbCr sampling {lh}x{lv}"))),
    };
    let missing = || Error::corrupt("Sony YCbCr: missing Huffman table");
    let ty = hdr.table(0).ok_or_else(missing)?;
    let tcb = hdr.table(n_pos - 2).ok_or_else(missing)?;
    let tcr = hdr.table(n_pos - 1).ok_or_else(missing)?;
    let base = 1i32 << (hdr.bits - 1);

    let mut out = vec![0u16; tile_w * tile_h * 3];
    let mut bits = BitPumpJpeg::new(&src[hdr.scan_offset..]);
    if n_pos == 6 {
        decode_420(&mut out, tile_w, tile_h, base, [ty, tcb, tcr], &mut bits)?;
    } else {
        decode_422(&mut out, tile_w, tile_h, base, [ty, tcb, tcr], &mut bits)?;
    }
    Ok(out)
}

/// Zero padding past the end of the entropy data that is tolerated: what one MCU can need at
/// most (`n_pos` codes of at most 16 + 16 bits) plus the 8 bytes the bit reader looks ahead.
fn overrun_allowance(n_pos: usize) -> usize {
    n_pos * 4 + 8
}

/// A tile whose reader has run further past the data than `limit` bytes is truncated.
#[inline(always)]
fn check_overrun(bits: &BitPumpJpeg, limit: usize) -> Result<()> {
    if bits.overrun() > limit { Err(Error::corrupt("truncated Sony YCbCr tile")) } else { Ok(()) }
}

fn decode_420(
    out: &mut [u16],
    w: usize,
    h: usize,
    base: i32,
    [ty, tcb, tcr]: [&HuffTable; 3],
    bits: &mut BitPumpJpeg,
) -> Result<()> {
    let stride = w * 3;
    let limit = overrun_allowance(6);
    let mut pred = [base; 3];
    for r in (0..h).step_by(2) {
        let top = r * stride;
        let bot = top + stride;
        for c in (0..w).step_by(2) {
            let o = c * 3;
            let (p1, p3);
            if c == 0 {
                if r > 0 {
                    let above = top - 2 * stride;
                    pred = [out[above] as i32, out[above + 1] as i32, out[above + 2] as i32];
                }
                p1 = pred[0];
                p3 = None;
            } else {
                // Left MCU: y2 on the top row, y4 on the bottom row, chroma on the top row.
                p1 = out[top + o - 3] as i32;
                p3 = Some(out[bot + o - 3] as i32);
                pred[1] = out[top + o - 2] as i32;
                pred[2] = out[top + o - 1] as i32;
            }
            let y1 = p1.wrapping_add(ty.decode_diff(bits)?);
            let y2 = y1.wrapping_add(ty.decode_diff(bits)?);
            let y3 = p3.unwrap_or(y1).wrapping_add(ty.decode_diff(bits)?);
            let y4 = y3.wrapping_add(ty.decode_diff(bits)?);
            let cb = pred[1].wrapping_add(tcb.decode_diff(bits)?);
            let cr = pred[2].wrapping_add(tcr.decode_diff(bits)?);
            let px = [(y1, top + o), (y2, top + o + 3), (y3, bot + o), (y4, bot + o + 3)];
            for (y, i) in px {
                out[i] = y as u16;
                out[i + 1] = cb as u16;
                out[i + 2] = cr as u16;
            }
        }
        check_overrun(bits, limit)?;
    }
    check_overrun(bits, limit)
}

fn decode_422(
    out: &mut [u16],
    w: usize,
    h: usize,
    base: i32,
    [ty, tcb, tcr]: [&HuffTable; 3],
    bits: &mut BitPumpJpeg,
) -> Result<()> {
    let stride = w * 3;
    let limit = overrun_allowance(4);
    let mut pred = [base; 3];
    for r in 0..h {
        let row = r * stride;
        for c in (0..w).step_by(2) {
            let o = row + c * 3;
            if c == 0 {
                if r > 0 {
                    let above = o - stride;
                    pred = [out[above] as i32, out[above + 1] as i32, out[above + 2] as i32];
                }
            } else {
                pred = [out[o - 3] as i32, out[o - 2] as i32, out[o - 1] as i32];
            }
            let y1 = pred[0].wrapping_add(ty.decode_diff(bits)?);
            let y2 = y1.wrapping_add(ty.decode_diff(bits)?);
            let cb = pred[1].wrapping_add(tcb.decode_diff(bits)?);
            let cr = pred[2].wrapping_add(tcr.decode_diff(bits)?);
            out[o] = y1 as u16;
            out[o + 1] = cb as u16;
            out[o + 2] = cr as u16;
            out[o + 3] = y2 as u16;
            out[o + 4] = cb as u16;
            out[o + 5] = cr as u16;
        }
        check_overrun(bits, limit)?;
    }
    check_overrun(bits, limit)
}

/// Convert one YCbCr sample (chroma centred on 16383) to clamped 16-bit RGB.
#[inline]
pub fn to_rgb(y: u16, cb: u16, cr: u16) -> [u16; 3] {
    let luma = f32::from(y);
    let blue_diff = (i32::from(cb) - CHROMA_CENTRE) as f32;
    let red_diff = (i32::from(cr) - CHROMA_CENTRE) as f32;
    let red = luma + R_FROM_CR * red_diff;
    let green = (luma - G_FROM_CB * blue_diff) - G_FROM_CR * red_diff;
    let blue = luma + B_FROM_CB * blue_diff;
    [clamp16(red), clamp16(green), clamp16(blue)]
}

#[inline(always)]
fn clamp16(q: f32) -> u16 {
    // NaN cannot occur (finite inputs); values are truncated toward zero.
    q.clamp(0.0, 65535.0) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgb_clamps_and_centres() {
        assert_eq!(to_rgb(1000, 16383, 16383), [1000, 1000, 1000]);
        assert_eq!(to_rgb(65535, 65535, 65535), [65535, 13518, 65535]);
        let low = to_rgb(0, 0, 0);
        assert!(low[0] == 0 && low[1] > 0 && low[2] == 0);
    }

    /// 5-bit flat code (symbol = category): an all-zero body decodes symbol 0 (diff 0) everywhere,
    /// so every sample equals the initial predictor.
    fn stream(w: u16, h: u16, hs: u8, body: &[u8]) -> Vec<u8> {
        let mut o = vec![0xFF, 0xD8, 0xFF, 0xC3, 0, 17, 16];
        o.extend(h.to_be_bytes());
        o.extend(w.to_be_bytes());
        o.extend([3, 1, hs << 4 | if hs == 2 { 2 } else { 1 }, 0, 2, 0x11, 0, 3, 0x11, 0]);
        let mut counts = [0u8; 16];
        counts[4] = 17;
        o.extend([0xFF, 0xC4, 0, 36, 0]);
        o.extend(counts);
        o.extend(0u8..17);
        o.extend([0xFF, 0xDA, 0, 12, 3, 1, 0, 2, 0, 3, 0, 1, 0, 0]);
        o.extend_from_slice(body);
        o.extend([0xFF, 0xD9]);
        o
    }

    #[test]
    fn flat_stream_gives_initial_predictor() {
        let out = decode_tile(&stream(4, 4, 2, &[0; 16]), 4, 4).unwrap();
        assert_eq!(out.len(), 48);
        assert!(out.iter().all(|&v| v == 32768));
    }

    #[test]
    fn rejects_bad_geometry_and_truncation() {
        assert!(decode_tile(&stream(4, 4, 2, &[0; 16]), 6, 4).is_err());
        assert!(decode_tile(&stream(4, 4, 2, &[]), 4, 4).is_ok());
        assert!(decode_tile(&stream(2, 200, 2, &[]), 2, 200).is_err());
    }
}
