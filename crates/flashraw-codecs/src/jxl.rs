//! JPEG XL decoding (DNG 1.7 compression 52546), via `jxl-oxide`.

use flashraw_core::{Error, Result};

/// A decoded JPEG XL tile with integer samples in the caller's level domain.
pub struct DecodedJxl {
    pub width: usize,
    pub height: usize,
    pub channels: usize,
    pub data: Vec<u16>,
}

/// Decode a JPEG XL stream to integers with `bits` bits per sample.
///
/// The codestream's own bit depth only says how precisely it stores the nominal [0, 1]
/// range; the container (DNG BitsPerSample) defines the level domain, so a 12-bit
/// codestream inside a 16-bit DNG maps 1.0 to 65535, not 4095.
pub fn decode(src: &[u8], bits: u32) -> Result<DecodedJxl> {
    let image = jxl_oxide::JxlImage::builder()
        .read(std::io::Cursor::new(src))
        .map_err(|e| Error::corrupt(format!("JPEG XL: {e}")))?;
    let render = image.render_frame(0).map_err(|e| Error::corrupt(format!("JPEG XL: {e}")))?;
    let fb = render.image_all_channels();
    let (w, h, c) = (fb.width(), fb.height(), fb.channels());
    let scale = level_scale(bits);
    let data = fb.buf().iter().map(|&v| to_level(v, scale)).collect();
    Ok(DecodedJxl { width: w, height: h, channels: c, data })
}

/// Largest level of a `bits`-bit container (1.0 in the codestream's nominal range).
fn level_scale(bits: u32) -> f32 {
    ((1u32 << bits.clamp(1, 16)) - 1) as f32
}

fn to_level(v: f32, scale: f32) -> u16 {
    (v * scale).round().clamp(0.0, 65535.0) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_follow_the_container_depth() {
        // A 12-bit codestream sample 528/4095 in a 16-bit DNG lands in the 16-bit domain.
        let v = 528.0 / 4095.0;
        assert_eq!(to_level(v, level_scale(16)), 8450);
        assert_eq!(to_level(1.0, level_scale(16)), 65535);
        assert_eq!(to_level(1.0, level_scale(12)), 4095);
        assert_eq!(to_level(-0.1, level_scale(16)), 0);
        assert_eq!(to_level(2.0, level_scale(16)), 65535);
        assert_eq!(level_scale(0), 1.0);
        assert_eq!(level_scale(32), 65535.0);
    }
}
