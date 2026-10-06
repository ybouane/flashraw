//! Preview fallback ([`DecodeOptions::preview_fallback`]): when the sensor data cannot be
//! decoded, RGB outputs can return the embedded full-size preview instead, clearly marked
//! as such ([`FrameSource::Preview`] plus a [`CorruptReason::PreviewFallback`] warning).

use crate::FlashRaw;
use crate::frame::{BackendReport, DecodeStatistics, FrameSource, RawFrame, RawPixelData};
use crate::options::{DecodeOptions, OutputFormat};
use flashraw_core::*;

/// Smallest preview accepted, as a fraction of the default crop in each dimension.
const MIN_COVERAGE: f64 = 0.9;

/// True when a failed decode should fall back to the preview: the caller opted in, the
/// sensor data is undecodable (unsupported compression) and an RGB output of the whole
/// image was requested.
pub(crate) fn applies(e: &Error, opts: &DecodeOptions) -> bool {
    opts.preview_fallback
        && matches!(e, Error::UnsupportedCompression { .. })
        && matches!(opts.output, OutputFormat::Rgba8 | OutputFormat::LinearRgbF32)
        && opts.region.is_none()
}

/// The largest decodable preview covering at least [`MIN_COVERAGE`] of `crop` in each
/// dimension (either way round: previews may be stored rotated).
pub(crate) fn full_size_preview(previews: &[PreviewInfo], crop: Rect) -> Option<&PreviewInfo> {
    let (cw, ch) = (crop.width as f64, crop.height as f64);
    let covers = |w: u32, h: u32| w as f64 >= cw * MIN_COVERAGE && h as f64 >= ch * MIN_COVERAGE;
    previews
        .iter()
        .filter(|p| matches!(p.encoding, PreviewEncoding::Jpeg | PreviewEncoding::Rgb8))
        .filter(|p| covers(p.width, p.height) || covers(p.height, p.width))
        .max_by_key(|p| (p.width as u64 * p.height as u64, p.length))
}

/// Decode the full-size preview as the frame for `opts`, or return `cause` when there is
/// none (or it does not decode).
pub(crate) fn preview_frame(raw: &FlashRaw<'_>, opts: &DecodeOptions, cause: Error) -> Result<RawFrame> {
    let t0 = flashraw_core::profile::now_ms();
    let info = raw.frame_info();
    let crop = if info.crop.is_empty() { Rect::full(info.width, info.height) } else { info.crop };
    let Some(p) = full_size_preview(raw.previews(), crop) else { return Err(cause) };
    if opts.limits.check_dims(p.width, p.height, 4).is_err() {
        return Err(cause);
    }
    let Ok(img) = raw.decode_preview_rgb(p) else { return Err(cause) };
    let (w, h) = (img.width as usize, img.height as usize);
    if w == 0 || h == 0 || img.rgb.len() < w * h * 3 {
        return Err(cause);
    }
    let (width, height, cpp, data, orientation) = match opts.output {
        OutputFormat::Rgba8 => {
            let (ow, oh, px) = orient_rgba8(&img.rgb, w, h, p.orientation);
            (ow, oh, 4, RawPixelData::U8(px), Orientation::Normal)
        }
        _ => {
            let lut: Vec<f32> = (0..256).map(|v| srgb_to_linear(v as f32 / 255.0)).collect();
            let rgb = img.rgb[..w * h * 3].iter().map(|&v| lut[v as usize]).collect();
            (w as u32, h as u32, 3, RawPixelData::F32(rgb), p.orientation)
        }
    };
    let total = flashraw_core::profile::now_ms() - t0 + raw.parse_ms();
    let stats = DecodeStatistics {
        backend: BackendReport {
            decode: "CPU (embedded preview)".into(),
            postprocess: None,
            simd: "scalar".into(),
            threads: 1,
            gpu_adapter: None,
        },
        stages: vec![(Stage::ContainerParse, raw.parse_ms()), (Stage::Postprocess, total - raw.parse_ms())],
        total_ms: total,
        input_bytes: raw.bytes().len() as u64,
        output_bytes: data.byte_len() as u64,
        opcodes: vec![],
    };
    let full = Rect::full(width, height);
    Ok(RawFrame {
        width,
        height,
        cpp,
        active_area: full,
        crop: full,
        origin: (0, 0),
        data,
        output: opts.output,
        bits_per_sample: 8,
        cfa: None,
        sensor_layout: SensorLayout::Linear,
        masked_areas: vec![],
        black_level: BlackLevel::uniform(0.0),
        white_level: WhiteLevel::uniform(255),
        white_balance: None,
        color_matrices: ColorMatrices::default(),
        orientation,
        metadata: raw.metadata().clone(),
        stats,
        warnings: vec![DecodeWarning::new(CorruptReason::PreviewFallback { message: cause.to_string() }, 0)],
        source: FrameSource::Preview,
    })
}

fn srgb_to_linear(v: f32) -> f32 {
    if v <= 0.040_45 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
}

/// Apply `o` to 8-bit RGB pixels, returning oriented RGBA8 (opaque).
fn orient_rgba8(rgb: &[u8], w: usize, h: usize, o: Orientation) -> (u32, u32, Vec<u8>) {
    let (ow, oh) = if o.swaps_dimensions() { (h, w) } else { (w, h) };
    let mut out = vec![0u8; ow * oh * 4];
    for oy in 0..oh {
        for ox in 0..ow {
            // Output pixel back to the stored pixel.
            let (sx, sy) = match o {
                Orientation::Normal => (ox, oy),
                Orientation::FlipH => (w - 1 - ox, oy),
                Orientation::Rotate180 => (w - 1 - ox, h - 1 - oy),
                Orientation::FlipV => (ox, h - 1 - oy),
                Orientation::Transpose => (oy, ox),
                Orientation::Rotate90 => (oy, h - 1 - ox),
                Orientation::Transverse => (w - 1 - oy, h - 1 - ox),
                Orientation::Rotate270 => (w - 1 - oy, ox),
            };
            let s = (sy * w + sx) * 3;
            let d = (oy * ow + ox) * 4;
            out[d..d + 3].copy_from_slice(&rgb[s..s + 3]);
            out[d + 3] = 255;
        }
    }
    (ow as u32, oh as u32, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preview(w: u32, h: u32, enc: PreviewEncoding) -> PreviewInfo {
        PreviewInfo::new(0, 1, w, h, enc, Orientation::Normal, PreviewKind::Preview)
    }

    #[test]
    fn only_full_size_previews_qualify() {
        let crop = Rect::new(0, 0, 8256, 5504);
        let small = preview(1620, 1080, PreviewEncoding::Jpeg);
        assert!(full_size_preview(std::slice::from_ref(&small), crop).is_none());
        let full = preview(8256, 5504, PreviewEncoding::Jpeg);
        let rotated = preview(5504, 8256, PreviewEncoding::Jpeg);
        let ps = [small.clone(), full.clone()];
        assert_eq!(full_size_preview(&ps, crop), Some(&full));
        assert_eq!(full_size_preview(std::slice::from_ref(&rotated), crop), Some(&rotated));
        // Slightly smaller than the crop (cameras trim a few pixels) still qualifies.
        let trimmed = preview(8000, 5400, PreviewEncoding::Jpeg);
        assert!(full_size_preview(std::slice::from_ref(&trimmed), crop).is_some());
    }

    #[test]
    fn fallback_needs_opt_in_rgb_output_and_unsupported_data() {
        let unsupported = Error::unsupported_compression("NEF", "test");
        let mut o = DecodeOptions { output: OutputFormat::Rgba8, ..Default::default() };
        assert!(!applies(&unsupported, &o), "off by default");
        o.preview_fallback = true;
        assert!(applies(&unsupported, &o));
        assert!(!applies(&Error::corrupt("x"), &o), "corrupt data is not substituted");
        o.output = OutputFormat::RawU16;
        assert!(!applies(&unsupported, &o), "sensor-domain outputs never fall back");
        o.output = OutputFormat::LinearRgbF32;
        assert!(applies(&unsupported, &o));
        o.region = Some(Rect::new(0, 0, 8, 8));
        assert!(!applies(&unsupported, &o), "region decodes never fall back");
    }

    #[test]
    fn orientation_of_preview_pixels() {
        // 2x1 image: red, green.
        let rgb = [255, 0, 0, 0, 255, 0];
        let (w, h, px) = orient_rgba8(&rgb, 2, 1, Orientation::Rotate90);
        assert_eq!((w, h), (1, 2));
        assert_eq!(&px[..4], &[255, 0, 0, 255]);
        assert_eq!(&px[4..], &[0, 255, 0, 255]);
        let (_, _, px) = orient_rgba8(&rgb, 2, 1, Orientation::FlipH);
        assert_eq!(&px[..4], &[0, 255, 0, 255]);
        assert_eq!(srgb_to_linear(1.0), 1.0);
        assert_eq!(srgb_to_linear(0.0), 0.0);
    }
}
