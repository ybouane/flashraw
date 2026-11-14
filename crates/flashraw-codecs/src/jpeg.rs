//! Lossy (DCT) JPEG (ITU-T T.81): DNG compression 34892 strips and tiles, and embedded previews.
//!
//! Decoding is done by `zune-jpeg`; this module chooses the output colour space, enforces the caller's limits and
//! maps errors. [`dimensions`] reads the frame size from the marker structure alone, without decoding.

use flashraw_core::{Error, Limits, Result};
use zune_jpeg::JpegDecoder;
use zune_jpeg::zune_core::colorspace::ColorSpace;
use zune_jpeg::zune_core::options::DecoderOptions;

/// An 8-bit image decoded from a JPEG stream.
pub struct Decoded8 {
    /// Width in pixels.
    pub width: usize,
    /// Height in pixels.
    pub height: usize,
    /// Samples per pixel: 1 (grey) or 3 (RGB).
    pub components: usize,
    /// `width * height * components` samples, row-major, interleaved.
    pub data: Vec<u8>,
}

/// The frame header fields X and Y are 16 bits wide (T.81 B.2.2).
const FORMAT_MAX_SIDE: u32 = 65_535;

fn decoder_error(e: impl core::fmt::Debug) -> Error {
    Error::corrupt(format!("JPEG: {e:?}"))
}

/// Decode a JPEG stream to interleaved 8-bit samples.
///
/// A stream whose frame has one component stays grey (one sample per pixel); anything else (YCbCr, RGB, CMYK,
/// YCCK) is converted to RGB by the decoder. The frame size is checked against `limits` as if it had three samples
/// per pixel. Errors: [`Error::CorruptFile`] for anything the decoder rejects (including a side above
/// `limits.max_dimension`, which the decoder enforces while reading the header), and the errors of
/// [`Limits::check_dims`].
pub fn decode(data: &[u8], limits: &Limits) -> Result<Decoded8> {
    let side = limits.max_dimension.min(FORMAT_MAX_SIDE) as usize;
    let options =
        DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGB).set_max_width(side).set_max_height(side);
    let mut decoder = JpegDecoder::new_with_options(data, options);
    decoder.decode_headers().map_err(decoder_error)?;
    let header = decoder.info().ok_or_else(|| Error::corrupt("JPEG: no header info"))?;
    limits.check_dims(u32::from(header.width), u32::from(header.height), 3)?;
    let grey = header.components == 1;
    let (pixels, size) = if grey {
        let mut luma = JpegDecoder::new_with_options(data, options.jpeg_set_out_colorspace(ColorSpace::Luma));
        (luma.decode().map_err(decoder_error)?, luma.dimensions())
    } else {
        (decoder.decode().map_err(decoder_error)?, decoder.dimensions())
    };
    let (width, height) = size.ok_or_else(|| Error::corrupt("JPEG: no dimensions"))?;
    Ok(Decoded8 { width, height, components: if grey { 1 } else { 3 }, data: pixels })
}

/// Width and height from the first frame header of a JPEG stream, without decoding.
///
/// `data` must start with SOI and may run past the end of the stream. Segments are skipped by their length until
/// the first start-of-frame marker; a start-of-scan marker, a length below 2 or fewer than 10 bytes left before a
/// marker give `None`. Fill bytes (0xFF) and stray bytes between segments are skipped; only SOI, TEM and RST0..7
/// are treated as markers without a length. Returns `(width, height)`.
pub fn dimensions(data: &[u8]) -> Option<(u32, u32)> {
    if data.get(..2) != Some(&[0xFF, 0xD8]) {
        return None;
    }
    let be16 = |at: usize| u32::from(data[at]) << 8 | u32::from(data[at + 1]);
    let mut at = 2usize;
    while at + 10 <= data.len() {
        if data[at] != 0xFF {
            at += 1;
            continue;
        }
        let code = data[at + 1];
        match code {
            0xFF => at += 1,
            0xD8 | 0x01 | 0xD0..=0xD7 => at += 2,
            0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF => return Some((be16(at + 7), be16(at + 5))),
            _ => {
                let length = be16(at + 2) as usize;
                if code == 0xDA || length < 2 {
                    return None;
                }
                at += 2 + length;
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOF0_100X50: [u8; 19] = [0xFF, 0xC0, 0, 17, 8, 0, 50, 0, 100, 3, 1, 0x11, 0, 2, 0x11, 1, 3, 0x11, 1];

    fn stream(parts: &[&[u8]]) -> Vec<u8> {
        let mut d = vec![0xFF, 0xD8];
        parts.iter().for_each(|p| d.extend_from_slice(p));
        d
    }

    #[test]
    fn frame_after_app_segment() {
        let app = [0xFF, 0xE1, 0, 6, 0xFF, 0xC0, 9, 9];
        assert_eq!(dimensions(&stream(&[&app, &SOF0_100X50])), Some((100, 50)));
    }

    #[test]
    fn scan_before_frame() {
        assert_eq!(dimensions(&stream(&[&[0xFF, 0xDA, 0, 2], &SOF0_100X50])), None);
    }

    #[test]
    fn missing_soi() {
        assert_eq!(dimensions(&SOF0_100X50), None);
        assert_eq!(dimensions(&[]), None);
    }

    #[test]
    fn fill_bytes_restart_markers_and_stray_bytes() {
        let d = stream(&[&[0xFF, 0xFF, 0xFF, 0xD3, 0x12, 0xFF, 0x01], &SOF0_100X50]);
        assert_eq!(dimensions(&d), Some((100, 50)));
    }

    #[test]
    fn progressive_frame_and_short_length() {
        let sof2 = [0xFF, 0xC2, 0, 0, 8, 0, 7, 0, 9, 1, 0];
        assert_eq!(dimensions(&stream(&[&sof2])), Some((9, 7)));
        assert_eq!(dimensions(&stream(&[&[0xFF, 0xE0, 0, 1], &SOF0_100X50])), None);
    }

    #[test]
    fn end_of_image_has_a_length() {
        // EOI is followed by a "length" like any other marker: here 4, which skips onto the frame header.
        assert_eq!(dimensions(&stream(&[&[0xFF, 0xD9, 0, 4, 0, 0], &SOF0_100X50])), Some((100, 50)));
    }

    #[test]
    fn slice_ends_inside_frame_header() {
        let d = stream(&[&SOF0_100X50]);
        assert_eq!(dimensions(&d[..11]), None);
        assert_eq!(dimensions(&d[..12]), Some((100, 50)));
    }

    #[test]
    fn garbage_is_corrupt() {
        let e = decode(b"not a jpeg at all", &Limits::default()).err().unwrap();
        assert_eq!(e.kind(), "CorruptFile");
        assert!(e.to_string().contains("JPEG: "));
        let e = decode(&stream(&[&SOF0_100X50]), &Limits::default()).err().unwrap();
        assert_eq!(e.kind(), "CorruptFile");
    }
}
