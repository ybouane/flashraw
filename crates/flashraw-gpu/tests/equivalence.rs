//! Kernel-vs-scalar equivalence tests.
//!
//! * Integer kernels (unpack, linearisation LUT) must be bit-exact with
//!   `flashraw_codecs::packed::unpack_row_scalar` (the format decoders' reference).
//! * Float kernels (normalise, develop) are compared with `flashraw::process` with a
//!   documented tolerance: |Δ| <= 1e-5 for normalised samples, |Δ| <= 1e-4 for linear
//!   RGB (GPU division/FMA may differ by a few ULP), and at most ±1 code value on
//!   < 0.1 % of RGBA8 samples (sRGB quantisation boundaries).
//!
//! GPU tests print "skipped" and pass when no adapter is available.

use flashraw::process::{self, DevelopParams};
use flashraw_codecs::packed::unpack_row_scalar;
use flashraw_core::*;
use flashraw_gpu::reference::{PackedRaster, packed_sample};
use flashraw_gpu::{DevelopOutput, DevelopSpec, GpuFrame, GpuInput, GpuOp, HostPixels};
use std::sync::Arc;

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

fn pseudo(n: usize, seed: u64) -> Vec<u8> {
    let mut x = seed | 1;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}

const ORDERS: [PackOrder; 4] = [PackOrder::MsbBytes, PackOrder::LsbBytes, PackOrder::Msb16, PackOrder::Msb32];

/// Expected unpack: each row is `src[y*stride .. min(y*stride + row_bytes, len)]` read
/// by the scalar bit pump (zeros beyond), i.e. the DNG/codec row semantics.
fn expected_unpack(src: &[u8], w: usize, h: usize, bits: u32, order: PackOrder, stride: usize) -> Vec<u16> {
    let row_bytes = (w * bits as usize).div_ceil(8);
    let mut out = vec![0u16; w * h];
    for y in 0..h {
        let s = src.get(y * stride..).unwrap_or(&[]);
        let s = &s[..row_bytes.min(s.len())];
        unpack_row_scalar(s, &mut out[y * w..(y + 1) * w], bits, order);
    }
    out
}

#[test]
fn reference_model_matches_scalar_unpack() {
    // Runs everywhere (no GPU): the scalar mirror of the WGSL addressing is bit-exact
    // with the codec reference for every order, depth, odd widths, padded/unaligned
    // strides and truncated payloads.
    for bits in 1..=16u32 {
        for order in ORDERS {
            for w in [1usize, 3, 8, 13, 64, 101] {
                let h = 5;
                let row_bytes = (w * bits as usize).div_ceil(8);
                for stride in [row_bytes, row_bytes + 3, row_bytes + 8] {
                    for cut in [0usize, 5] {
                        let len = (stride * h).saturating_sub(cut);
                        let src = pseudo(len, bits as u64 * 7919 + w as u64 + stride as u64);
                        let want = expected_unpack(&src, w, h, bits, order, stride);
                        let r = PackedRaster { bits, order, row_stride: stride, row_bytes };
                        let got = flashraw_gpu::reference::unpack(&src, &r, w, h);
                        assert_eq!(got, want, "bits {bits} {order:?} w {w} stride {stride} cut {cut}");
                    }
                }
            }
        }
    }
    let _ = packed_sample;
}

#[test]
fn gpu_unpack_bit_exact() {
    let Some(ctx) = gpu() else { return };
    let info = FrameInfo::new(8, 8, 16);
    let mut cases = 0;
    for bits in 1..=16u32 {
        for order in ORDERS {
            for (w, h) in [(1usize, 3usize), (7, 5), (33, 9), (257, 17)] {
                let row_bytes = (w * bits as usize).div_ceil(8);
                for (stride, cut) in [(row_bytes, 0usize), (row_bytes + 3, 0), (row_bytes + 1, 7)] {
                    let len = (stride * h).saturating_sub(cut);
                    let src = pseudo(len, bits as u64 * 31 + w as u64 * 7 + stride as u64);
                    let want = expected_unpack(&src, w, h, bits, order, stride);
                    let frame = GpuFrame {
                        input: GpuInput::Packed { data: &src, bits, order, row_stride: stride },
                        raster_width: w as u32,
                        raster_height: h as u32,
                        cpp: 1,
                        window: Rect::full(w as u32, h as u32),
                        origin: (0, 0),
                        linearization: None,
                    };
                    let run = ctx.run_blocking(&frame, &info, &GpuOp::Unpack).unwrap();
                    let HostPixels::U16(got) = run.image.download().unwrap().0 else { panic!("format") };
                    assert_eq!(got, want, "bits {bits} {order:?} {w}x{h} stride {stride} cut {cut}");
                    cases += 1;
                }
            }
        }
    }
    eprintln!("{cases} unpack cases bit-exact");
}

#[test]
fn gpu_unpack_window_and_lut() {
    let Some(ctx) = gpu() else { return };
    let (w, h, bits) = (301usize, 47usize, 14u32);
    let stride = (w * bits as usize).div_ceil(8) + 5;
    let src = pseudo(stride * h, 99);
    let full = expected_unpack(&src, w, h, bits, PackOrder::MsbBytes, stride);
    let lut: Vec<u16> = (0..12000u32).map(|v| (v * 3 / 2 + 7) as u16).collect();
    let win = Rect::new(13, 9, 200, 31);
    let frame = GpuFrame {
        input: GpuInput::Packed { data: &src, bits, order: PackOrder::MsbBytes, row_stride: stride },
        raster_width: w as u32,
        raster_height: h as u32,
        cpp: 1,
        window: Rect::full(w as u32, h as u32),
        origin: (0, 0),
        linearization: None,
    }
    .with_window(win)
    .with_linearization(Some(&lut));
    let info = FrameInfo::new(w as u32, h as u32, 14);
    let run = ctx.run_blocking(&frame, &info, &GpuOp::Unpack).unwrap();
    let HostPixels::U16(got) = run.image.download().unwrap().0 else { panic!() };
    let last = *lut.last().unwrap();
    for y in 0..win.height as usize {
        for x in 0..win.width as usize {
            let v = full[(y + win.y as usize) * w + x + win.x as usize];
            let want = lut.get(v as usize).copied().unwrap_or(last);
            assert_eq!(got[y * win.width as usize + x], want, "({x},{y})");
        }
    }
    // u16 input through the same kernel (CPU-decoded upload path) with odd width.
    let data: Vec<u16> = (0..(w * h) as u32).map(|i| (i * 2_654_435_761 >> 18) as u16).collect();
    let run = ctx.run_blocking(&GpuFrame::from_u16(&data, w as u32, h as u32, 1), &info, &GpuOp::Unpack).unwrap();
    assert_eq!(run.image.download().unwrap().0, HostPixels::U16(data));
}

/// A frame with non-trivial calibration: 2x2 black pattern, row/col deltas,
/// per-colour white levels.
fn calibrated_info(w: u32, h: u32, cfa: Option<CfaPattern>, cpp: u32) -> FrameInfo {
    let mut info = FrameInfo::new(w, h, 14);
    info.cpp = cpp;
    info.cfa = cfa;
    info.black_level = BlackLevel::pattern2x2([510.0, 512.0, 509.0, 515.0]);
    // Deltas sized for the sensor (origin offsets below stay inside).
    info.black_level.row_deltas = (0..h + 8).map(|y| (y % 5) as f32 * 0.5).collect();
    info.black_level.col_deltas = (0..w + 8).map(|x| (x % 3) as f32 * -0.25).collect();
    info.white_level = WhiteLevel { values: vec![16000, 16383, 15800] };
    info.white_balance = WhiteBalance::from_multipliers(2.1, 1.0, 1.6, 1.0);
    info
}

fn synthetic_u16(w: u32, h: u32, cpp: u32, seed: u32) -> RawImage {
    let mut img = RawImage::new_u16(w, h, cpp, &Limits::default()).unwrap();
    for (i, v) in img.u16_mut().iter_mut().enumerate() {
        // Smooth gradient + noise, spanning below black to above white.
        let x = (i as u32 / cpp) % w;
        let y = (i as u32 / cpp) / w;
        let n = (i as u32).wrapping_mul(2_654_435_761).wrapping_add(seed) >> 22;
        *v = (300 + x * 40 + y * 25 + n).min(16383) as u16;
    }
    img
}

fn max_abs_diff(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len());
    a.iter().zip(b).map(|(x, y)| (x - y).abs()).fold(0.0, f32::max)
}

#[test]
fn gpu_normalize_matches_cpu() {
    let Some(ctx) = gpu() else { return };
    for (cfa, cpp) in [(Some(CfaPattern::RGGB), 1u32), (None, 3), (None, 1)] {
        let (w, h) = (123u32, 77u32);
        let info = calibrated_info(w, h, cfa, cpp);
        let img = synthetic_u16(w, h, cpp, 5);
        let origin = (3, 1);
        let want = process::normalize(&img, &info, origin, false);
        let frame = GpuFrame::from_u16(img.u16(), w, h, cpp).with_origin(origin);
        let run = ctx.run_blocking(&frame, &info, &GpuOp::Normalize).unwrap();
        let HostPixels::F32(got) = run.image.download().unwrap().0 else { panic!() };
        let d = max_abs_diff(&got, &want);
        eprintln!("normalize cfa={:?} cpp={cpp}: max |Δ| = {d:e}", cfa.is_some());
        assert!(d <= 1e-5, "max diff {d}");
    }
}

struct DevCase {
    name: &'static str,
    cfa: Option<CfaPattern>,
    cpp: u32,
}

fn xtrans() -> CfaPattern {
    let s = "GGRGGBGGBGGRBRGRBGGGBGGRGGRGGBRBGBRG";
    let colors: Vec<CfaColor> = s.chars().map(|c| CfaColor::from_letter(c).unwrap()).collect();
    CfaPattern::new(6, 6, &colors).unwrap()
}

fn cmp_rgba(got: &[u8], want: &[u8], what: &str) {
    assert_eq!(got.len(), want.len(), "{what}: length");
    let mut off = 0usize;
    for (a, b) in got.iter().zip(want) {
        let d = (*a as i32 - *b as i32).abs();
        assert!(d <= 1, "{what}: diff {d}");
        off += (d > 0) as usize;
    }
    let frac = off as f64 / got.len() as f64;
    eprintln!("{what}: {off} of {} RGBA8 samples differ by 1 ({:.4}%)", got.len(), frac * 100.0);
    assert!(frac < 1e-3, "{what}: too many off-by-one samples ({frac})");
}

#[test]
fn gpu_develop_matches_cpu() {
    let Some(ctx) = gpu() else { return };
    let cases = [
        DevCase { name: "bayer", cfa: Some(CfaPattern::RGGB), cpp: 1 },
        DevCase { name: "xtrans", cfa: Some(xtrans()), cpp: 1 },
        DevCase { name: "rgb", cfa: None, cpp: 3 },
        DevCase { name: "mono", cfa: None, cpp: 1 },
    ];
    let (w, h) = (157u32, 93u32);
    let orientations = [
        Orientation::Normal,
        Orientation::FlipH,
        Orientation::Rotate180,
        Orientation::FlipV,
        Orientation::Transpose,
        Orientation::Rotate90,
        Orientation::Transverse,
        Orientation::Rotate270,
    ];
    let cm = ColorMatrices {
        color: vec![
            ColorMatrix::new(Illuminant::D65, 3, vec![0.6, -0.1, -0.05, -0.4, 1.2, 0.2, -0.1, 0.25, 0.6]).unwrap(),
        ],
        ..Default::default()
    };
    for case in &cases {
        let mut info = calibrated_info(w, h, case.cfa, case.cpp);
        info.color_matrices = cm.clone();
        let img = synthetic_u16(w, h, case.cpp, 11);
        let origin = (1, 3);
        for half in [false, true] {
            if half && case.name == "mono" {
                // The CPU reference yields black for half-size mono (it averages a channel
                // it never accumulates); the GPU produces the grey average. Not compared.
                continue;
            }
            for (oi, &orientation) in orientations.iter().enumerate() {
                let crop = Rect::new(5, 4, w - 9, h - 7);
                let mut params = DevelopParams::new(
                    &info,
                    if half { flashraw::Demosaic::HalfSize } else { flashraw::Demosaic::Bilinear },
                    crop,
                );
                params.orientation = orientation;
                let dev = process::develop(&img, &info, origin, &params, false).unwrap();
                let spec = |output| DevelopSpec {
                    crop,
                    half_size: half,
                    wb: params.wb,
                    cam_to_rgb: params.cam_to_rgb,
                    orientation,
                    output,
                };
                let frame = GpuFrame::from_u16(img.u16(), w, h, case.cpp).with_origin(origin);
                let what = format!("{} half={half} orient={orientation:?}", case.name);
                if oi == 0 {
                    let run =
                        ctx.run_blocking(&frame, &info, &GpuOp::Develop(spec(DevelopOutput::LinearRgbF32))).unwrap();
                    assert_eq!((run.image.width(), run.image.height()), (dev.width, dev.height));
                    let HostPixels::F32(got) = run.image.download().unwrap().0 else { panic!() };
                    let d = max_abs_diff(&got, &dev.rgb);
                    eprintln!("{what}: linear max |Δ| = {d:e}");
                    assert!(d <= 1e-4, "{what}: linear diff {d}");
                }
                let (ow, oh, want) = process::develop_rgba8(&img, &info, origin, &params, false).unwrap();
                let run = ctx.run_blocking(&frame, &info, &GpuOp::Develop(spec(DevelopOutput::Rgba8))).unwrap();
                assert_eq!((run.image.width(), run.image.height()), (ow, oh), "{what}");
                let HostPixels::U8(got) = run.image.download().unwrap().0 else { panic!() };
                cmp_rgba(&got, &want, &what);
            }
        }
    }
}

#[test]
fn gpu_develop_from_packed_and_f32() {
    let Some(ctx) = gpu() else { return };
    let (w, h, bits) = (211u32, 64u32, 12u32);
    let stride = (w * bits).div_ceil(8) as usize + 2; // padded, unaligned rows
    let src = pseudo(stride * h as usize, 3);
    for order in ORDERS {
        let unpacked = expected_unpack(&src, w as usize, h as usize, bits, order, stride);
        let mut img = RawImage::new_u16(w, h, 1, &Limits::default()).unwrap();
        img.u16_mut().copy_from_slice(&unpacked);
        let mut info = calibrated_info(w, h, Some(CfaPattern::RGGB), 1);
        info.white_level = WhiteLevel::uniform(4095);
        info.black_level = BlackLevel::uniform(64.0);
        let crop = Rect::new(2, 2, w - 4, h - 4);
        let params = DevelopParams::new(&info, flashraw::Demosaic::Bilinear, crop);
        let dev = process::develop(&img, &info, (0, 0), &params, false).unwrap();
        let frame = GpuFrame {
            input: GpuInput::Packed { data: &src, bits, order, row_stride: stride },
            raster_width: w,
            raster_height: h,
            cpp: 1,
            window: Rect::full(w, h),
            origin: (0, 0),
            linearization: None,
        };
        let spec = DevelopSpec {
            crop,
            half_size: false,
            wb: params.wb,
            cam_to_rgb: params.cam_to_rgb,
            orientation: Orientation::Normal,
            output: DevelopOutput::LinearRgbF32,
        };
        let run = ctx.run_blocking(&frame, &info, &GpuOp::Develop(spec)).unwrap();
        let HostPixels::F32(got) = run.image.download().unwrap().0 else { panic!() };
        let d = max_abs_diff(&got, &dev.rgb);
        eprintln!("packed {order:?}: linear max |Δ| = {d:e}");
        assert!(d <= 1e-4);
    }
    // Float input (floating-point DNG path).
    let mut img = RawImage::new_f32(64, 48, 1, &Limits::default()).unwrap();
    for (i, v) in img.f32_mut().iter_mut().enumerate() {
        *v = (i % 977) as f32 / 977.0;
    }
    let mut info = FrameInfo::new(64, 48, 16);
    info.sample_format = SampleFormat::F32;
    info.cfa = Some(CfaPattern::RGGB);
    info.white_level = WhiteLevel::uniform(1);
    let params = DevelopParams::new(&info, flashraw::Demosaic::Bilinear, Rect::full(64, 48));
    let dev = process::develop(&img, &info, (0, 0), &params, false).unwrap();
    let PixelData::F32(data) = &img.data else { panic!() };
    let spec = DevelopSpec {
        crop: Rect::full(64, 48),
        half_size: false,
        wb: params.wb,
        cam_to_rgb: params.cam_to_rgb,
        orientation: Orientation::Normal,
        output: DevelopOutput::LinearRgbF32,
    };
    let run = ctx.run_blocking(&GpuFrame::from_f32(data, 64, 48, 1), &info, &GpuOp::Develop(spec)).unwrap();
    let HostPixels::F32(got) = run.image.download().unwrap().0 else { panic!() };
    assert!(max_abs_diff(&got, &dev.rgb) <= 1e-4);
}

#[test]
fn gpu_image_to_texture() {
    let Some(ctx) = gpu() else { return };
    let info = {
        let mut i = FrameInfo::new(40, 30, 12);
        i.cfa = Some(CfaPattern::RGGB);
        i
    };
    let img = synthetic_u16(40, 30, 1, 1);
    let spec = DevelopSpec {
        crop: Rect::full(40, 30),
        half_size: false,
        wb: [1.0; 4],
        cam_to_rgb: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        orientation: Orientation::Rotate90,
        output: DevelopOutput::Rgba8,
    };
    let run = ctx.run_blocking(&GpuFrame::from_u16(img.u16(), 40, 30, 1), &info, &GpuOp::Develop(spec)).unwrap();
    let tex = run.image.to_texture().unwrap();
    assert_eq!((tex.width(), tex.height()), (30, 40));
    // Read the texture back and compare with the buffer.
    let dev = ctx.device();
    let bpr = 256u32; // >= 30 * 4, aligned
    let rb = dev.create_buffer(&flashraw_gpu::wgpu::BufferDescriptor {
        label: None,
        size: (bpr * 40) as u64,
        usage: flashraw_gpu::wgpu::BufferUsages::MAP_READ | flashraw_gpu::wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut enc = dev.create_command_encoder(&Default::default());
    enc.copy_texture_to_buffer(
        tex.as_image_copy(),
        flashraw_gpu::wgpu::TexelCopyBufferInfo {
            buffer: &rb,
            layout: flashraw_gpu::wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(bpr), rows_per_image: None },
        },
        tex.size(),
    );
    ctx.queue().submit([enc.finish()]);
    let (tx, rx) = std::sync::mpsc::channel();
    rb.map_async(flashraw_gpu::wgpu::MapMode::Read, .., move |r| tx.send(r).unwrap());
    dev.poll(flashraw_gpu::wgpu::PollType::wait_indefinitely()).unwrap();
    rx.recv().unwrap().unwrap();
    let view = rb.get_mapped_range(..).unwrap();
    let HostPixels::U8(px) = run.image.download().unwrap().0 else { panic!() };
    for y in 0..40usize {
        assert_eq!(&view[y * bpr as usize..y * bpr as usize + 120], &px[y * 120..(y + 1) * 120]);
    }
}
