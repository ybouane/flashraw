//! Baseline (lossy) JPEG decoding for lossy DNG tiles and preview conversion.

use flashraw_core::{Error, Limits, Result};

/// A decoded 8-bit image.
pub struct Decoded8 {
    pub width: usize,
    pub height: usize,
    pub components: usize,
    pub data: Vec<u8>,
}

/// Decode a baseline/progressive JPEG to 8-bit samples, keeping the native colour
/// components (1 for grey, 3 for RGB). The declared dimensions are checked against
/// `limits` before the decoder allocates its output.
pub fn decode(data: &[u8], limits: &Limits) -> Result<Decoded8> {
    use zune_jpeg::zune_core::colorspace::ColorSpace;
    use zune_jpeg::zune_core::options::DecoderOptions;
    // The format itself cannot exceed 65535 per side.
    let cap = limits.max_dimension.min(65535) as usize;
    let options = |cs| DecoderOptions::default().jpeg_set_out_colorspace(cs).set_max_width(cap).set_max_height(cap);
    let mut d = zune_jpeg::JpegDecoder::new_with_options(data, options(ColorSpace::RGB));
    d.decode_headers().map_err(|e| Error::corrupt(format!("JPEG: {e:?}")))?;
    let info = d.info().ok_or_else(|| Error::corrupt("JPEG: no header info"))?;
    let gray = info.components == 1;
    limits.check_dims(u32::from(info.width), u32::from(info.height), 3)?;
    if gray {
        // Headers only so far, so switching the output colour space costs a header re-parse.
        d = zune_jpeg::JpegDecoder::new_with_options(data, options(ColorSpace::Luma));
    }
    let pixels = d.decode().map_err(|e| Error::corrupt(format!("JPEG: {e:?}")))?;
    let (w, h) = d.dimensions().ok_or_else(|| Error::corrupt("JPEG: no dimensions"))?;
    Ok(Decoded8 { width: w, height: h, components: if gray { 1 } else { 3 }, data: pixels })
}

/// Read only the dimensions of a JPEG (SOFn scan), without decoding.
pub fn dimensions(data: &[u8]) -> Option<(u32, u32)> {
    let mut p = 2usize;
    if data.get(0..2)? != [0xFF, 0xD8] {
        return None;
    }
    while p + 9 < data.len() {
        if data[p] != 0xFF {
            p += 1;
            continue;
        }
        let m = data[p + 1];
        if m == 0xFF {
            p += 1;
            continue;
        }
        if matches!(m, 0xD8 | 0x01 | 0xD0..=0xD7) {
            p += 2;
            continue;
        }
        let len = u16::from_be_bytes([data[p + 2], data[p + 3]]) as usize;
        if matches!(m, 0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF) {
            let h = u16::from_be_bytes([data[p + 5], data[p + 6]]) as u32;
            let w = u16::from_be_bytes([data[p + 7], data[p + 8]]) as u32;
            return Some((w, h));
        }
        if m == 0xDA || len < 2 {
            return None;
        }
        p += 2 + len;
    }
    None
}
