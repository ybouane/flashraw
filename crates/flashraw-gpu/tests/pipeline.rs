//! End-to-end: `FlashRaw::decode` with `Backend::Gpu` vs `Backend::Cpu` on real files.
//!
//! Files: every `*.dng` in `$FLASHRAW_GPU_SAMPLES` (default: `<workspace>/samples`).
//! Skipped when there is no GPU adapter or no sample directory.

use flashraw::{Backend, DecodeOptions, Demosaic, FlashRaw, OutputFormat, RawPixelData, Rect, Stage};
use std::path::PathBuf;

fn samples() -> Vec<PathBuf> {
    let dir = std::env::var_os("FLASHRAW_GPU_SAMPLES")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../samples"));
    let mut v: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map(|d| {
            d.filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("dng")))
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
            assert!(gpu.stats.backend.decode.starts_with("GPU"), "{}", gpu.stats.backend.decode);
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
