//! JPEG XL payloads (ISO/IEC 18181; DNG 1.7 compression 52546).
//!
//! Each strip or tile is a complete JPEG XL image (bare codestream or container), decoded by `jxl-oxide`. The
//! first frame is rendered with the decoder's defaults and its nominal range [0, 1] is mapped onto the integer
//! levels of the IFD's BitsPerSample.

use flashraw_core::{Error, Result};

/// A decoded JPEG XL image: interleaved integer samples in the container's level range.
pub struct DecodedJxl {
    /// Width in pixels.
    pub width: usize,
    /// Height in pixels.
    pub height: usize,
    /// Samples per pixel: colour channels followed by extra channels.
    pub channels: usize,
    /// `width * height * channels` samples, row-major, interleaved.
    pub data: Vec<u16>,
}

fn decoder_error(e: impl core::fmt::Display) -> Error {
    Error::corrupt(format!("JPEG XL: {e}"))
}

/// `v * scale` rounded to the nearest integer (halves away from zero) and clamped to `[0, 65535]`; NaN gives 0.
#[inline(always)]
fn to_level(v: f32, scale: f32) -> u16 {
    let x = v * scale;
    // `as` truncates toward zero and saturates (NaN and negatives give 0), so `whole` is floor(x) for x in
    // [0, 65535). The fraction is exact there; anything at or above 65535 ends at the clamp.
    let whole = (x as u32).min(65_535);
    let up = u32::from(x - whole as f32 >= 0.5);
    (whole + up).min(65_535) as u16
}

/// Decode a JPEG XL stream to integer samples for a container of `bits` bits per sample.
///
/// The first frame is rendered with every channel; each value `v` becomes `v * (2^b - 1)` rounded to the nearest
/// integer and clamped to 16 bits, with `b` = `bits` limited to 1..=16. Any decoder failure is
/// [`Error::CorruptFile`].
pub fn decode(src: &[u8], bits: u32) -> Result<DecodedJxl> {
    let image = jxl_oxide::JxlImage::builder().read(src).map_err(decoder_error)?;
    let render = image.render_frame(0).map_err(decoder_error)?;
    let frame = render.image_all_channels();
    let scale = ((1u32 << bits.clamp(1, 16)) - 1) as f32;
    let data = frame.buf().iter().map(|&v| to_level(v, scale)).collect();
    Ok(DecodedJxl { width: frame.width(), height: frame.height(), channels: frame.channels(), data })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scale(bits: u32) -> f32 {
        ((1u32 << bits.clamp(1, 16)) - 1) as f32
    }

    #[test]
    fn level_mapping() {
        assert_eq!(to_level(528.0 / 4095.0, scale(16)), 8450);
        assert_eq!(to_level(1.0, scale(16)), 65535);
        assert_eq!(to_level(1.0, scale(12)), 4095);
        assert_eq!(to_level(-0.1, scale(16)), 0);
        assert_eq!(to_level(2.0, scale(16)), 65535);
        assert_eq!(to_level(f32::NAN, scale(16)), 0);
        assert_eq!(to_level(f32::INFINITY, scale(16)), 65535);
        assert_eq!(to_level(f32::NEG_INFINITY, scale(16)), 0);
        assert_eq!(to_level(0.5, scale(1)), 1);
        assert_eq!(to_level(0.49, scale(1)), 0);
        assert_eq!(to_level(0.5, scale(0)), 1);
        assert_eq!(scale(0), 1.0);
        assert_eq!(scale(32), 65535.0);
        // Halves away from zero, just below a half rounds down.
        assert_eq!(to_level(2.5, 1.0), 3);
        assert_eq!(to_level(f32::from_bits(0.5f32.to_bits() - 1), 1.0), 0);
        assert_eq!(to_level(65534.5, 1.0), 65535);
        assert_eq!(to_level(70000.0, 1.0), 65535);
    }

    #[test]
    fn level_mapping_matches_rounding() {
        let mut x = 0x1234_5678u32;
        for _ in 0..200_000 {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            let v = f32::from_bits(x);
            for s in [1.0, 4095.0, 65535.0] {
                let p = v * s;
                let want = if p.is_nan() { 0 } else { p.round().clamp(0.0, 65535.0) as u16 };
                assert_eq!(to_level(v, s), want, "{v:e} * {s}");
            }
            let v = (x >> 8) as f32 / (1u32 << 23) as f32 - 0.25;
            for s in [1.0, 255.0, 4095.0, 65535.0] {
                assert_eq!(to_level(v, s), (v * s).round().clamp(0.0, 65535.0) as u16, "{v:e} * {s}");
            }
        }
    }

    #[test]
    fn garbage_is_corrupt() {
        for input in [&b""[..], b"\xFF\x0A", b"not a JPEG XL image"] {
            let e = decode(input, 16).err().unwrap();
            assert_eq!(e.kind(), "CorruptFile");
            assert!(e.to_string().contains("JPEG XL: "));
        }
    }
}
