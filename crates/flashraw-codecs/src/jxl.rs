//! JPEG XL decoding (DNG 1.7 compression 52546), via `jxl-oxide`.

use flashraw_core::{Error, Result};

/// A decoded JPEG XL tile with integer samples at the stream's bit depth.
pub struct DecodedJxl {
    pub width: usize,
    pub height: usize,
    pub channels: usize,
    pub data: Vec<u16>,
}

pub fn decode(src: &[u8]) -> Result<DecodedJxl> {
    let image = jxl_oxide::JxlImage::builder()
        .read(std::io::Cursor::new(src))
        .map_err(|e| Error::corrupt(format!("jxl: {e}")))?;
    let bits = image.image_header().metadata.bit_depth.bits_per_sample().clamp(1, 16);
    let render = image.render_frame(0).map_err(|e| Error::corrupt(format!("jxl: {e}")))?;
    let fb = render.image_all_channels();
    let (w, h, c) = (fb.width(), fb.height(), fb.channels());
    let scale = ((1u32 << bits) - 1) as f32;
    let data = fb.buf().iter().map(|&v| (v * scale).round().clamp(0.0, 65535.0) as u16).collect();
    Ok(DecodedJxl { width: w, height: h, channels: c, data })
}
