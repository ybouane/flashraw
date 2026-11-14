//! GPU AHD demosaic (`Demosaic::Ahd`) against the CPU implementation on synthetic Bayer mosaics:
//! the four phases, a sensor origin that is not a multiple of the pattern, crops, a region
//! window, small frames, several bands and the 8-bit output.
//!
//! The CPU result is the reference. Same algorithm and operation order, but the GPU may contract
//! multiply-adds and round `sqrt` and division differently, and AHD decides with thresholds
//! (lightness and chroma steps): a rounding difference can change the homogeneity count of a
//! pixel and with it the blend of its two candidates, which moves the pixel by more than a
//! rounding error. Measured on an Apple M1 Ultra (Metal): mean |diff| 2.6e-5 of linear float
//! output, 0.35 % to 0.75 % of the `Rgba8` samples different, by up to 25 codes; on Mesa's
//! llvmpipe (Vulkan, software) no difference at all. Bounds: mean |diff| <= 1e-4, max |diff|
//! <= 0.25, `Rgba8` samples different < 1.5 % and by at most 64 codes.
//!
//! Prints "skipped" and passes when there is no GPU adapter.

use flashraw::process::{self, DevelopParams};
use flashraw_core::*;
use flashraw_gpu::{DevelopOutput, DevelopSpec, GpuFrame, GpuOp, HostPixels};
use std::sync::{Arc, Mutex, MutexGuard};

fn serial() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn gpu() -> Option<Arc<flashraw_gpu::GpuContext>> {
    match flashraw_gpu::context() {
        Ok(c) => {
            eprintln!("GPU: {}", c.info());
            Some(c)
        }
        Err(e) => {
            eprintln!("skipped: {e}");
            None
        }
    }
}

const BLACK: f32 = 256.0;
const WHITE: u32 = 4095;

/// Sharp coloured shapes (edges at all angles, thin lines), mosaicked with `cfa`.
fn mosaic(w: u32, h: u32, cfa: &CfaPattern, origin: (u32, u32)) -> RawImage {
    let mut img = RawImage::new_u16(w, h, 1, &Limits::default()).unwrap();
    for y in 0..h {
        for x in 0..w {
            let (fx, fy) = (x as f32, y as f32);
            let disc = ((fx - 70.0).powi(2) + (fy - 60.0).powi(2)).sqrt() < 48.0;
            let diag = (fx * 0.8 + fy * 0.6) as i32 % 18 < 6;
            let line = (fy - 25.0).abs() < 0.8 || (fx - 150.0).abs() < 0.8;
            let rgb = match (disc, diag, line) {
                (_, _, true) => [0.9, 0.1, 0.1],
                (true, true, _) => [0.1, 0.8, 0.3],
                (true, false, _) => [0.2, 0.3, 0.9],
                (false, true, _) => [0.8, 0.7, 0.2],
                _ => [0.05 + 0.4 * fx / w as f32, 0.3, 0.3 + 0.002 * fy],
            };
            let c = cfa.color_at(x as usize + origin.0 as usize, y as usize + origin.1 as usize) as usize;
            img.u16_mut()[(y * w + x) as usize] = (BLACK + rgb[c] * (WHITE as f32 - BLACK)) as u16;
        }
    }
    img
}

fn frame_info(w: u32, h: u32, cfa: CfaPattern) -> FrameInfo {
    let mut info = FrameInfo::new(w, h, 12);
    info.cfa = Some(cfa);
    info.black_level = BlackLevel::uniform(BLACK);
    info.white_level = WhiteLevel::uniform(WHITE);
    info.white_balance = WhiteBalance::from_multipliers(2.0, 1.0, 1.6, 1.0);
    info
}

/// Develop `window` of `img` on the CPU and on the GPU with AHD and compare. Returns the bands.
fn compare(
    ctx: &Arc<flashraw_gpu::GpuContext>,
    what: &str,
    img: &RawImage,
    cfa: CfaPattern,
    origin: (u32, u32),
    window: Rect,
    crop: Rect,
) -> u32 {
    let w = img.width;
    let info = frame_info(window.width, window.height, cfa);
    let mut sub = RawImage::new_u16(window.width, window.height, 1, &Limits::default()).unwrap();
    for y in 0..window.height {
        let src = &img.u16()[((window.y + y) * w + window.x) as usize..][..window.width as usize];
        sub.u16_mut()[(y * window.width) as usize..][..window.width as usize].copy_from_slice(src);
    }
    let sub_origin = (origin.0 + window.x, origin.1 + window.y);
    let mut params = DevelopParams::new(&info, flashraw::Demosaic::Ahd, crop);
    params.cam_to_rgb = [[1.5, -0.3, -0.2], [-0.2, 1.4, -0.2], [0.05, -0.4, 1.35]];
    let dev = process::develop(&sub, &info, sub_origin, &params, false).unwrap();
    if window.width >= 100 {
        let bil = DevelopParams { demosaic: flashraw::Demosaic::Bilinear, ..params.clone() };
        let bil = process::develop(&sub, &info, sub_origin, &bil, false).unwrap();
        assert!(bil.rgb.iter().zip(&dev.rgb).any(|(a, b)| (a - b).abs() > 1e-2), "{what}: AHD equals bilinear");
    }
    let ahd = flashraw::gpu::ahd_spec(&info, sub_origin).unwrap_or_else(|| panic!("{what}: no AHD spec"));
    let frame = GpuFrame::from_u16(img.u16(), w, img.height, 1).with_origin(origin).with_window(window);
    let spec = |output| DevelopSpec {
        crop,
        half_size: false,
        wb: params.wb,
        cam_to_rgb: params.cam_to_rgb,
        curve: None,
        orientation: Orientation::Normal,
        output,
        levels: None,
        edge: Some(ahd),
    };
    let run = ctx.run_blocking(&frame, &info, &GpuOp::Develop(spec(DevelopOutput::LinearRgbF32))).unwrap();
    let bands = run.timings.bands;
    assert_eq!((run.image.width(), run.image.height()), (dev.width, dev.height), "{what}");
    let HostPixels::F32(got) = run.image.download().unwrap().0 else { panic!("format") };
    let (mut max, mut sum) = (0f32, 0f64);
    for (a, b) in got.iter().zip(&dev.rgb) {
        assert!(a.is_finite(), "{what}: non-finite GPU value");
        max = max.max((a - b).abs());
        sum += (a - b).abs() as f64;
    }
    let mean = sum / got.len() as f64;
    let far = got.iter().zip(&dev.rgb).filter(|(a, b)| (*a - *b).abs() > 1e-3).count();
    eprintln!(
        "{what}: bands {bands} max |diff| {max:.2e} mean {mean:.2e}, {far} of {} samples off by > 1e-3",
        got.len()
    );
    assert!(max <= 0.25, "{what}: max diff {max}");
    assert!(mean <= 1e-4, "{what}: mean diff {mean}");
    let (ow, oh, want) = process::develop_rgba8(&sub, &info, sub_origin, &params, false).unwrap();
    let run = ctx.run_blocking(&frame, &info, &GpuOp::Develop(spec(DevelopOutput::Rgba8))).unwrap();
    assert_eq!((run.image.width(), run.image.height()), (ow, oh), "{what}");
    let HostPixels::U8(got) = run.image.download().unwrap().0 else { panic!("format") };
    let off = got.iter().zip(&want).filter(|(a, b)| a != b).count();
    let worst = got.iter().zip(&want).map(|(a, b)| (*a as i32 - *b as i32).abs()).max().unwrap_or(0);
    eprintln!("{what}: rgba8 {off} of {} samples differ, by at most {worst}", got.len());
    assert!(worst <= 64, "{what}: rgba8 differs by {worst}");
    assert!(off * 200 < got.len() * 3, "{what}: {off} of {} samples differ", got.len());

    bands
}

#[test]
fn gpu_ahd_matches_cpu() {
    let _serial = serial();
    let Some(ctx) = gpu() else { return };
    let (w, h) = (203u32, 141u32);
    for (name, cfa) in
        [("RGGB", CfaPattern::RGGB), ("GBRG", CfaPattern::GBRG), ("BGGR", CfaPattern::BGGR), ("GRBG", CfaPattern::GRBG)]
    {
        for origin in [(0u32, 0u32), (1, 3)] {
            let img = mosaic(w, h, &cfa, origin);
            let what = format!("{name} origin {origin:?}");
            compare(&ctx, &what, &img, cfa, origin, Rect::full(w, h), Rect::new(5, 4, w - 9, h - 7));
            compare(
                &ctx,
                &format!("{what} window"),
                &img,
                cfa,
                origin,
                Rect::new(13, 9, 150, 111),
                Rect::full(150, 111),
            );
            for (sw, sh) in [(12u32, 9u32), (13, 13), (14, 15), (19, 40)] {
                let win = Rect::new(7, 5, sw, sh);
                compare(&ctx, &format!("{what} {sw}x{sh}"), &img, cfa, origin, win, Rect::full(sw, sh));
            }
        }
    }
}

#[test]
fn gpu_ahd_bands_match_the_single_band_result() {
    let _serial = serial();
    let Some(ctx) = gpu() else { return };
    let (w, h) = (203u32, 141u32);
    let img = mosaic(w, h, &CfaPattern::RGGB, (0, 0));
    ctx.override_limits(Some(200_000), Some(200_000));
    let bands =
        compare(&ctx, "banded", &img, CfaPattern::RGGB, (0, 0), Rect::full(w, h), Rect::new(3, 6, w - 8, h - 10));
    ctx.override_limits(None, None);
    assert!(bands > 4, "bands {bands}");
}
