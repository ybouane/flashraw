//! End-to-end: `FlashRaw::decode` with `Backend::Gpu` vs `Backend::Cpu` on real files.
//!
//! Files: every file in `$FLASHRAW_GPU_SAMPLES`, or every `*.dng` in
//! `<workspace>/samples` by default. Skipped when there is no GPU adapter or no samples.

use flashraw::{Backend, DecodeOptions, Demosaic, FlashRaw, OutputFormat, RawPixelData, Rect, Stage};
use std::path::PathBuf;

fn samples() -> Vec<PathBuf> {
    let env = std::env::var_os("FLASHRAW_GPU_SAMPLES").map(PathBuf::from);
    let all = env.is_some();
    let dir = env.unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../samples"));
    let mut v: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map(|d| {
            d.filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.is_file() && (all || p.extension().is_some_and(|e| e.eq_ignore_ascii_case("dng"))))
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

fn ready() -> Option<Vec<PathBuf>> {
    if !flashraw::gpu_available() {
        eprintln!("skipped: no GPU adapter");
        return None;
    }
    let s = samples();
    if s.is_empty() {
        eprintln!("skipped: no samples (set FLASHRAW_GPU_SAMPLES)");
        return None;
    }
    Some(s)
}

fn opts(backend: Backend, output: OutputFormat) -> DecodeOptions {
    DecodeOptions { backend, output, ..Default::default() }
}

fn rgba_close(a: &[u8], b: &[u8], what: &str) {
    assert_eq!(a.len(), b.len(), "{what}");
    let mut off = 0usize;
    for (x, y) in a.iter().zip(b) {
        let d = (*x as i32 - *y as i32).abs();
        assert!(d <= 1, "{what}: |Δ| = {d}");
        off += (d != 0) as usize;
    }
    let frac = off as f64 / a.len() as f64;
    eprintln!("  {what}: {off} samples off by one ({:.5}%)", frac * 100.0);
    assert!(frac < 1e-3, "{what}");
}

#[test]
fn gpu_backend_matches_cpu_on_files() {
    let Some(files) = ready() else { return };
    for path in files {
        let raw = FlashRaw::open_path(&path).unwrap();
        let caps = raw.capabilities();
        eprintln!(
            "{} ({}x{}, gpu_unpack={})",
            path.display(),
            raw.frame_info().width,
            raw.frame_info().height,
            caps.gpu_unpack
        );

        // Raw u16 straight from the packed payload: bit-exact with the CPU decoder.
        if caps.gpu_unpack {
            let cpu = raw.decode(opts(Backend::Cpu, OutputFormat::RawU16)).unwrap();
            let gpu = raw.decode(opts(Backend::Gpu, OutputFormat::RawU16)).unwrap();
            // Frames whose black level is measured from masked pixels are decoded on the CPU.
            if !raw.frame_info().black_from_masked {
                assert!(gpu.stats.backend.decode.starts_with("GPU"), "{}", gpu.stats.backend.decode);
            }
            assert_eq!(cpu.as_u16().unwrap(), gpu.as_u16().unwrap(), "raw u16");
            let reg = Rect::new(333, 211, 1001, 517);
            let cpu = raw.decode_region(reg, opts(Backend::Cpu, OutputFormat::RawU16)).unwrap();
            let gpu = raw.decode_region(reg, opts(Backend::Gpu, OutputFormat::RawU16)).unwrap();
            assert_eq!(cpu.as_u16().unwrap(), gpu.as_u16().unwrap(), "raw u16 region");
        }

        // Full RGBA8 develop.
        for demosaic in [Demosaic::Bilinear, Demosaic::HalfSize] {
            let o = DecodeOptions { demosaic, ..opts(Backend::Cpu, OutputFormat::Rgba8) };
            let cpu = raw.decode(o.clone()).unwrap();
            let gpu = raw.decode(DecodeOptions { backend: Backend::Gpu, ..o }).unwrap();
            assert_eq!((cpu.width, cpu.height), (gpu.width, gpu.height));
            assert!(gpu.stats.stage_ms(Stage::GpuCompute) > 0.0);
            eprintln!("  stats: {:?} | {:?}", gpu.stats.backend.postprocess, gpu.stats.stages);
            rgba_close(cpu.as_u8().unwrap(), gpu.as_u8().unwrap(), &format!("rgba8 {demosaic:?}"));
        }

        // 16-bit and half-float RGB (computed as 4-sample pixels, downloaded as 3), with levels.
        for output in [OutputFormat::Rgb16, OutputFormat::LinearRgb16, OutputFormat::LinearRgbF16] {
            let o = DecodeOptions { exposure: 0.5, black_point: 0.01, ..opts(Backend::Cpu, output) };
            let cpu = raw.decode(o.clone()).unwrap();
            let gpu = raw.decode(DecodeOptions { backend: Backend::Gpu, ..o }).unwrap();
            assert_eq!((cpu.width, cpu.height, cpu.cpp, cpu.orientation), (gpu.width, gpu.height, 3, gpu.orientation));
            let (a, b, half) = match (&cpu.data, &gpu.data) {
                (RawPixelData::U16(a), RawPixelData::U16(b)) => (a, b, false),
                (RawPixelData::F16(a), RawPixelData::F16(b)) => (a, b, true),
                _ => panic!("{output}: sample types differ"),
            };
            // Half floats compare in ULPs, ordered by their sign-magnitude bits (-0 == +0).
            let key = |h: u16| if half && h & 0x8000 != 0 { -((h & 0x7fff) as i32) } else { h as i32 };
            let off = a.iter().zip(b).filter(|(x, y)| key(**x) != key(**y)).count();
            let worst = a.iter().zip(b).map(|(x, y)| (key(*x) - key(*y)).abs()).max().unwrap_or(0);
            eprintln!("  {output}: {off} samples differ (max {worst} steps)");
            assert!(worst <= 1 && (off as f64) < a.len() as f64 * 1e-3, "{output}");
        }

        // Region decodes with an odd origin: linear RGB and normalised samples.
        let reg = Rect::new(1001, 503, 1200, 800);
        let cpu = raw.decode_region(reg, opts(Backend::Cpu, OutputFormat::LinearRgbF32)).unwrap();
        let gpu = raw.decode_region(reg, opts(Backend::Gpu, OutputFormat::LinearRgbF32)).unwrap();
        let d = cpu.as_f32().unwrap().iter().zip(gpu.as_f32().unwrap()).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
        eprintln!("  linear region max |Δ| = {d:e}");
        assert!(d <= 1e-4);
        let cpu = raw.decode_region(reg, opts(Backend::Cpu, OutputFormat::NormalizedF32)).unwrap();
        let gpu = raw.decode_region(reg, opts(Backend::Gpu, OutputFormat::NormalizedF32)).unwrap();
        let d = cpu.as_f32().unwrap().iter().zip(gpu.as_f32().unwrap()).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
        assert!(d <= 1e-5, "normalized {d}");

        // GPU-resident output, then download: identical to the downloaded variant.
        let resident = raw.decode(opts(Backend::Auto, OutputFormat::GpuRgba8)).unwrap();
        let g = resident.as_gpu().expect("gpu data").clone();
        assert_eq!(g.len(), resident.data.len());
        assert_eq!(resident.stats.stage_ms(Stage::GpuDownload), 0.0);
        let host = resident.download().unwrap();
        assert_eq!(host.output, OutputFormat::Rgba8);
        let direct = raw.decode(opts(Backend::Gpu, OutputFormat::Rgba8)).unwrap();
        assert_eq!(host.as_u8(), direct.as_u8());
        assert!(matches!(host.data, RawPixelData::U8(_)));

        if let Some(d) = flashraw::gpu::auto_decision(&raw, &opts(Backend::Auto, OutputFormat::Rgba8)) {
            eprintln!("  auto (rgba8): gpu={} ({})", d.use_gpu, d.reason);
        }

        // Forced tiling (16 MB bindings): row bands into one output buffer, then one
        // buffer per band. Bit-identical to the untiled GPU result.
        if direct.data.byte_len() <= 32 << 20 {
            continue;
        }
        let ctx = flashraw::gpu::context().unwrap();
        let half =
            raw.decode(DecodeOptions { demosaic: Demosaic::HalfSize, ..opts(Backend::Gpu, OutputFormat::Rgba8) });
        let half = half.unwrap();
        let unpacked = caps.gpu_unpack.then(|| raw.decode(opts(Backend::Gpu, OutputFormat::RawU16)).unwrap());
        let direct16 = raw.decode(opts(Backend::Gpu, OutputFormat::Rgb16)).unwrap();
        for (label, buffer) in [("subrange", None), ("tiled", Some(16u64 << 20))] {
            ctx.override_limits(Some(16 << 20), buffer);
            let t = raw.decode(opts(Backend::Gpu, OutputFormat::Rgba8)).unwrap();
            let post = t.stats.backend.postprocess.clone().unwrap_or_default();
            eprintln!("  {label}: {post}");
            assert!(post.contains("bands"), "{post}");
            assert!(t.as_u8() == direct.as_u8(), "{label}: tiled rgba8 differs");
            let t16 = raw.decode(opts(Backend::Gpu, OutputFormat::Rgb16)).unwrap();
            assert!(t16.as_u16() == direct16.as_u16(), "{label}: tiled rgb16 differs");
            let r = raw.decode(opts(Backend::Gpu, OutputFormat::GpuRgba8)).unwrap();
            let g = r.as_gpu().unwrap();
            assert_eq!(g.is_tiled(), buffer.is_some() || raw.frame_info().orientation.swaps_dimensions());
            assert!(r.download().unwrap().as_u8() == direct.as_u8(), "{label}: tiled resident differs");
            let o = DecodeOptions { demosaic: Demosaic::HalfSize, ..opts(Backend::Gpu, OutputFormat::Rgba8) };
            assert!(raw.decode(o).unwrap().as_u8() == half.as_u8(), "{label}: tiled half-size differs");
            if let Some(u) = &unpacked {
                let t = raw.decode(opts(Backend::Gpu, OutputFormat::RawU16)).unwrap();
                assert!(t.as_u16() == u.as_u16(), "{label}: tiled unpack differs");
            }
        }
        ctx.override_limits(None, None);
    }
}

#[test]
fn resident_outputs_require_gpu_backend() {
    let Some(files) = ready() else { return };
    let raw = FlashRaw::open_path(&files[0]).unwrap();
    assert!(raw.decode(opts(Backend::Cpu, OutputFormat::GpuRgba8)).is_err());
    // Default decode (Auto + RawU16) stays on the CPU.
    let f = raw.decode(DecodeOptions::default()).unwrap();
    assert!(f.stats.backend.gpu_adapter.is_none());
}

/// Colour spaces end to end on the bundled fixture (needs a GPU adapter, not the samples):
/// the GPU backend matches the CPU in every built-in space, GPU-resident outputs honour the
/// space, and an output ICC profile falls back to the CPU (resident outputs refuse it).
#[test]
fn gpu_color_spaces_and_profile_fallback() {
    if !flashraw::gpu_available() {
        eprintln!("skipped: no GPU adapter");
        return;
    }
    use flashraw::{ColorSpace, OutputProfile};
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../npm/tests/fixtures/canon-5d3-mlv.dng");
    let raw = FlashRaw::open_path(path).unwrap();
    for cs in ColorSpace::ALL {
        let o =
            DecodeOptions { color_space: cs, demosaic: Demosaic::HalfSize, ..opts(Backend::Cpu, OutputFormat::Rgba8) };
        let cpu = raw.decode(o.clone()).unwrap();
        let gpu = raw.decode(DecodeOptions { backend: Backend::Gpu, ..o.clone() }).unwrap();
        assert!(gpu.stats.stage_ms(Stage::GpuCompute) > 0.0, "{cs:?} ran on the GPU");
        assert_eq!((gpu.color_space, gpu.icc_profile()), (cs, Some(&cs.icc_profile()[..])));
        rgba_close(cpu.as_u8().unwrap(), gpu.as_u8().unwrap(), &format!("rgba8 {cs:?}"));
        let res = raw.decode(DecodeOptions { backend: Backend::Gpu, output: OutputFormat::GpuRgba8, ..o }).unwrap();
        assert_eq!(res.color_space, cs);
        let host = res.download().unwrap();
        rgba_close(cpu.as_u8().unwrap(), host.as_u8().unwrap(), &format!("gpu-rgba8 {cs:?}"));
    }
    let p = OutputProfile::new(ColorSpace::DisplayP3.icc_profile());
    let o = DecodeOptions {
        output_profile: Some(p),
        demosaic: Demosaic::HalfSize,
        ..opts(Backend::Gpu, OutputFormat::Rgba8)
    };
    let f = raw.decode(o.clone()).unwrap();
    assert!(f.stats.backend.gpu_adapter.is_none(), "profile conversion runs on the CPU");
    assert_eq!(f.color_space, ColorSpace::Custom);
    let res = DecodeOptions { output: OutputFormat::GpuRgba8, ..o };
    assert!(matches!(raw.decode(res), Err(flashraw::Error::InvalidArgument(_))));
}
