//! GPU edge-aware demosaic (`Demosaic::EdgeAware`) against the CPU implementation on synthetic
//! mosaics: Bayer in the three phases used by the corpus and X-Trans (also shifted), clean frames
//! (smoothing weight beta = 0) and noisy ones (beta > 0), crops, region windows, a sensor origin
//! that is not a multiple of the pattern, frames processed in several bands, and the 8-bit output.
//!
//! The CPU result is the reference. The two are the same algorithm with the same operation order;
//! what differs is fused multiply-add contraction, division and `sqrt` rounding of the GPU, which
//! the direction weights (`1 / (E + 1e-3)^2`) amplify where the gradients are tiny. Tolerance for
//! linear float output: max |diff| <= 5e-4 and mean |diff| <= 1e-6. Measured on an Apple M1 Ultra
//! (Metal): max 1.3e-6, mean 5e-8, i.e. a few ULP of f32; the bound leaves room for adapters whose
//! division and `sqrt` are less exact. `Rgba8`: at most +-1 code on < 0.2 % of the samples.
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

/// Sharp coloured shapes (edges at all angles, thin lines), with photon-like noise of variance
/// `noise^2 * value` when `noise > 0`, mosaicked with `cfa` (sample (0, 0) at sensor position
/// `origin`).
fn mosaic(w: u32, h: u32, cfa: &CfaPattern, origin: (u32, u32), noise: f32, seed: u32) -> RawImage {
    let mut s = seed;
    let mut uni = move || {
        s = s.wrapping_mul(1664525).wrapping_add(1013904223);
        ((s >> 8) as f32 + 0.5) / (1u32 << 24) as f32
    };
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
            let mut v = rgb[c];
            if noise > 0.0 {
                // Sum of uniforms is close enough to a Gaussian here.
                let g = (uni() + uni() + uni() + uni() - 2.0) * 1.732;
                v += g * (noise * noise * v).sqrt();
            }
            img.u16_mut()[(y * w + x) as usize] = (BLACK + v.clamp(0.0, 1.0) * (WHITE as f32 - BLACK)) as u16;
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

struct Stats {
    max: f32,
    mean: f64,
}

fn diff(a: &[f32], b: &[f32]) -> Stats {
    assert_eq!(a.len(), b.len());
    let mut max = 0f32;
    let mut sum = 0f64;
    for (x, y) in a.iter().zip(b) {
        assert!(x.is_finite(), "non-finite GPU value");
        let d = (x - y).abs();
        max = max.max(d);
        sum += d as f64;
    }
    Stats { max, mean: sum / a.len() as f64 }
}

fn patterns() -> Vec<(&'static str, CfaPattern)> {
    let xt = CfaPattern::parse("GGRGGB/GGBGGR/BRGRBG/GGBGGR/GGRGGB/RBGBRG").unwrap();
    vec![
        ("RGGB", CfaPattern::RGGB),
        ("GBRG", CfaPattern::GBRG),
        ("BGGR", CfaPattern::BGGR),
        ("xtrans", xt),
        ("xtrans shifted", xt.shifted(2, 1)),
    ]
}

/// Develop `window` of `img` (a `w` x `h` raster whose sample (0, 0) is at sensor position
/// `origin`) on the CPU and on the GPU and compare. Returns the smoothing weight used.
#[allow(clippy::too_many_arguments)]
fn compare(
    ctx: &Arc<flashraw_gpu::GpuContext>,
    what: &str,
    img: &RawImage,
    cfa: CfaPattern,
    origin: (u32, u32),
    window: Rect,
    crop: Rect,
    check_u8: bool,
) -> (f32, u32) {
    let (w, h) = (img.width, img.height);
    let info = frame_info(window.width, window.height, cfa);
    // The CPU develops the window as an image of its own whose origin is moved.
    let mut sub = RawImage::new_u16(window.width, window.height, 1, &Limits::default()).unwrap();
    for y in 0..window.height {
        let src = &img.u16()[((window.y + y) * w + window.x) as usize..][..window.width as usize];
        sub.u16_mut()[(y * window.width) as usize..][..window.width as usize].copy_from_slice(src);
    }
    let _ = h;
    let sub_origin = (origin.0 + window.x, origin.1 + window.y);
    let mut params = DevelopParams::new(&info, flashraw::Demosaic::EdgeAware, crop);
    params.cam_to_rgb = [[1.5, -0.3, -0.2], [-0.2, 1.4, -0.2], [0.05, -0.4, 1.35]];
    let dev = process::develop(&sub, &info, sub_origin, &params, false).unwrap();
    if window.width >= 100 {
        // The test is only meaningful if edge-aware differs from the bilinear develop.
        let bil = DevelopParams { demosaic: flashraw::Demosaic::Bilinear, ..params.clone() };
        let bil = process::develop(&sub, &info, sub_origin, &bil, false).unwrap();
        assert!(diff(&bil.rgb, &dev.rgb).max > 1e-2, "{what}: edge-aware equals bilinear");
    }
    let norm = process::normalize(&sub, &info, sub_origin, false);
    let (sw, sh) = (window.width as usize, window.height as usize);
    let edge = flashraw::gpu::edge_spec(&info, sub_origin, (sw, sh), params.wb, &mut |y, row| {
        row.copy_from_slice(&norm[y * sw..(y + 1) * sw])
    })
    .unwrap_or_else(|| panic!("{what}: no edge plan"));
    let beta = edge.beta;
    let frame = GpuFrame::from_u16(img.u16(), w, h, 1).with_origin(origin).with_window(window);
    let spec = |output| DevelopSpec {
        crop,
        half_size: false,
        wb: params.wb,
        cam_to_rgb: params.cam_to_rgb,
        curve: None,
        orientation: Orientation::Normal,
        output,
        levels: None,
        edge: Some(edge),
    };
    let run = ctx.run_blocking(&frame, &info, &GpuOp::Develop(spec(DevelopOutput::LinearRgbF32))).unwrap();
    let bands = run.timings.bands;
    assert_eq!((run.image.width(), run.image.height()), (dev.width, dev.height), "{what}");
    let HostPixels::F32(got) = run.image.download().unwrap().0 else { panic!("format") };
    let d = diff(&got, &dev.rgb);
    eprintln!("{what}: beta {beta:.3} bands {} max |diff| {:.2e} mean {:.2e}", run.timings.bands, d.max, d.mean);
    assert!(d.max <= 5e-4, "{what}: max diff {}", d.max);
    assert!(d.mean <= 1e-6, "{what}: mean diff {}", d.mean);
    if check_u8 {
        let (ow, oh, want) = process::develop_rgba8(&sub, &info, sub_origin, &params, false).unwrap();
        let run = ctx.run_blocking(&frame, &info, &GpuOp::Develop(spec(DevelopOutput::Rgba8))).unwrap();
        assert_eq!((run.image.width(), run.image.height()), (ow, oh), "{what}");
        let HostPixels::U8(got) = run.image.download().unwrap().0 else { panic!("format") };
        let off = got.iter().zip(&want).filter(|(a, b)| a != b).count();
        assert!(got.iter().zip(&want).all(|(a, b)| (*a as i32 - *b as i32).abs() <= 1), "{what}: rgba8 differs by > 1");
        assert!(off * 500 < got.len(), "{what}: {off} of {} samples differ", got.len());
    }
    (beta, bands)
}

#[test]
fn gpu_edge_aware_matches_cpu() {
    let _serial = serial();
    let Some(ctx) = gpu() else { return };
    let (w, h) = (203u32, 141u32);
    let full = Rect::full(w, h);
    let mut betas = [0usize; 2];
    for (name, cfa) in patterns() {
        for origin in [(0u32, 0u32), (1, 3)] {
            for noise in [0.0f32, 0.04] {
                let img = mosaic(w, h, &cfa, origin, noise, 7 + origin.0 + noise.to_bits());
                let crop = Rect::new(5, 4, w - 9, h - 7);
                let what = format!("{name} origin {origin:?} noise {noise}");
                let (beta, _) = compare(&ctx, &what, &img, cfa, origin, full, crop, true);
                betas[(beta > 0.0) as usize] += 1;
                assert_eq!(beta > 0.0, noise > 0.0, "{what}: beta {beta}");
            }
        }
    }
    eprintln!("cases with beta = 0: {}, beta > 0: {}", betas[0], betas[1]);
    assert!(betas[0] > 0 && betas[1] > 0);
}

#[test]
fn gpu_edge_aware_window_and_small_frames() {
    let _serial = serial();
    let Some(ctx) = gpu() else { return };
    let (w, h) = (203u32, 141u32);
    for (name, cfa) in patterns() {
        let img = mosaic(w, h, &cfa, (2, 1), 0.04, 21);
        // A region of the raster: the halo is measured from the window border.
        let win = Rect::new(13, 9, 150, 111);
        compare(&ctx, &format!("{name} window"), &img, cfa, (2, 1), win, Rect::full(150, 111), false);
        // Frames too small for any interior pixel, and just large enough for a few.
        for (sw, sh) in [(12u32, 9u32), (17, 17), (18, 18), (19, 40)] {
            let win = Rect::new(7, 5, sw, sh);
            compare(&ctx, &format!("{name} {sw}x{sh}"), &img, cfa, (2, 1), win, Rect::full(sw, sh), false);
        }
    }
}

#[test]
fn gpu_edge_aware_bands_match_the_single_band_result() {
    let _serial = serial();
    let Some(ctx) = gpu() else { return };
    let (w, h) = (203u32, 141u32);
    for (name, cfa) in [patterns().remove(0), patterns().remove(3)] {
        let img = mosaic(w, h, &cfa, (0, 0), 0.04, 5);
        let crop = Rect::new(3, 6, w - 8, h - 10);
        // A binding of 200 KB allows 12 developed rows per band: ten and more bands.
        ctx.override_limits(Some(200_000), Some(200_000));
        let (beta, bands) = compare(&ctx, &format!("{name} banded"), &img, cfa, (0, 0), Rect::full(w, h), crop, true);
        ctx.override_limits(None, None);
        assert!(beta > 0.0 && bands > 4, "bands {bands}");
    }
}
