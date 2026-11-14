//! CPU vs GPU benchmark on real files (equivalent work on both sides: the same
//! bilinear demosaic / unpack semantics, verified by the equivalence tests).
//!
//! ```text
//! cargo bench -p flashraw-gpu --bench gpu_pipeline -- [FILES...]
//! ```
//! Without arguments, every `*.dng` in `$FLASHRAW_GPU_SAMPLES` (default `<workspace>/samples`).
//! `FLASHRAW_BENCH_ITERS` sets iterations (default 7, median reported).
//! `FLASHRAW_BENCH_BAND_MB=N` caps storage bindings at N MB so frames run in bands
//! (one output buffer bound per band); add `FLASHRAW_BENCH_TILED=1` to also cap buffers,
//! which makes GPU-resident outputs tiled (one buffer per band).
//!
//! Cases per file:
//! * `unpack`: RAW u16 output. CPU = SIMD unpack (all threads); GPU = upload packed
//!   bytes + unpack kernel + download; GPU resident = without download.
//! * `rgba8`: full RAW → sRGB RGBA8 (bilinear). CPU fused develop vs GPU fused kernel,
//!   with and without download.
//! * `rgba8 half`: same with 2x2 superpixel.

use flashraw::{Backend, DecodeOptions, Demosaic, FlashRaw, OutputFormat, Stage};
use std::path::PathBuf;

fn files() -> Vec<PathBuf> {
    let args: Vec<PathBuf> = std::env::args().skip(1).filter(|a| !a.starts_with("--")).map(PathBuf::from).collect();
    if !args.is_empty() {
        return args;
    }
    let dir = std::env::var_os("FLASHRAW_GPU_SAMPLES")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../samples"));
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|d| {
            d.filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("dng")))
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

struct Sample {
    bands: u32,
    wall: f64,
    upload: f64,
    compute: f64,
    download: f64,
    kernel: Option<f64>,
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

fn bench(raw: &FlashRaw<'_>, opts: &DecodeOptions, iters: usize) -> Option<Sample> {
    raw.decode(opts.clone()).ok()?; // warm-up (pipeline compile, allocations)
    let mut s = Vec::new();
    for _ in 0..iters {
        let t = std::time::Instant::now();
        let f = raw.decode(opts.clone()).ok()?;
        let wall = t.elapsed().as_secs_f64() * 1e3;
        let kernel = f.stats.backend.postprocess.as_deref().and_then(|p| {
            let i = p.find("kernel ")?;
            p[i + 7..].split(' ').next()?.parse::<f64>().ok()
        });
        let bands = f.stats.backend.postprocess.as_deref().and_then(|p| {
            let i = p.find(" bands")?;
            p[..i].rsplit(' ').next()?.parse::<u32>().ok()
        });
        s.push(Sample {
            bands: bands.unwrap_or(1),
            wall,
            upload: f.stats.stage_ms(Stage::GpuUpload),
            compute: f.stats.stage_ms(Stage::GpuCompute),
            download: f.stats.stage_ms(Stage::GpuDownload),
            kernel,
        });
        drop(f);
    }
    let m = |f: &dyn Fn(&Sample) -> f64| median(s.iter().map(f).collect());
    Some(Sample {
        bands: s.iter().map(|x| x.bands).max().unwrap_or(1),
        wall: m(&|x| x.wall),
        upload: m(&|x| x.upload),
        compute: m(&|x| x.compute),
        download: m(&|x| x.download),
        kernel: s.iter().all(|x| x.kernel.is_some()).then(|| m(&|x| x.kernel.unwrap_or(0.0))),
    })
}

fn main() {
    // One-time costs a fresh process pays (device creation, calibration incl. pipeline compiles).
    let t = std::time::Instant::now();
    let ctx = flashraw_gpu::context();
    let init_ms = t.elapsed().as_secs_f64() * 1e3;
    if let Ok(ctx) = &ctx {
        let t = std::time::Instant::now();
        let g = ctx.calibration_blocking();
        let cal_ms = t.elapsed().as_secs_f64() * 1e3;
        let t = std::time::Instant::now();
        let c = flashraw::gpu::cpu_costs();
        let cpu_ms = t.elapsed().as_secs_f64() * 1e3;
        println!(
            "one-time: device init {init_ms:.1} ms, GPU calibration {cal_ms:.1} ms, CPU calibration {cpu_ms:.1} ms"
        );
        println!(
            "GPU calibration: upload {:.1} GB/s, download {:.1} GB/s, develop {:.0} MP/s, half {:.0} MP/s, unpack {:.0} MP/s, overhead {:.2} ms (unified memory: {})",
            g.upload_bytes_per_ms / 1e6,
            g.download_bytes_per_ms / 1e6,
            g.develop_px_per_ms / 1e3,
            g.develop_half_px_per_ms / 1e3,
            g.unpack_px_per_ms / 1e3,
            g.overhead_ms,
            ctx.unified_memory()
        );
        println!(
            "CPU calibration: develop {:.0} MP/s, half {:.0} MP/s, unpack {:.0} MP/s, normalize {:.0} MP/s",
            c.develop_px_per_ms / 1e3,
            c.develop_half_px_per_ms / 1e3,
            c.unpack_px_per_ms / 1e3,
            c.normalize_px_per_ms / 1e3
        );
    }
    if !flashraw::gpu_available() {
        eprintln!("no GPU adapter: nothing to benchmark");
        return;
    }
    if let Ok(ctx) = &ctx {
        println!(
            "adapter limits: storage binding {:.2} GB, buffer {:.2} GB",
            ctx.info().max_storage_binding as f64 / 1e9,
            ctx.info().max_buffer_size as f64 / 1e9
        );
    }
    if let (Ok(ctx), Some(mb)) =
        (&ctx, std::env::var("FLASHRAW_BENCH_BAND_MB").ok().and_then(|v| v.parse::<u64>().ok()))
    {
        let tiled = std::env::var("FLASHRAW_BENCH_TILED").is_ok_and(|v| v == "1");
        ctx.override_limits(Some(mb << 20), tiled.then_some(mb << 20));
        println!("forced bands: storage bindings capped at {mb} MB (tiled outputs: {tiled})");
    }
    let iters: usize = std::env::var("FLASHRAW_BENCH_ITERS").ok().and_then(|v| v.parse().ok()).unwrap_or(7);
    let info = flashraw_gpu::adapter_info().unwrap();
    println!("GPU: {info}  |  CPU threads: {}  |  median of {iters}", flashraw::low_level::core::par::threads());
    println!();
    println!(
        "| File | MP | Payload | Case | CPU ms | GPU ms (download) | GPU ms (resident) | Auto ms (choice) | GPU stages up/compute/kernel/down | Bands |"
    );
    println!("|---|---|---|---|---|---|---|---|---|---|");
    for path in files() {
        let Ok(raw) = FlashRaw::open_path(&path) else { continue };
        let fi = raw.frame_info();
        let mp = fi.width as f64 * fi.height as f64 / 1e6;
        let packed = raw.capabilities().gpu_unpack;
        let name = path.file_name().unwrap().to_string_lossy().chars().take(24).collect::<String>();
        let payload =
            if packed { format!("packed {}-bit", fi.bits_per_sample) } else { format!("{:?}", fi.compression) };
        let cases: Vec<(&str, OutputFormat, OutputFormat, Demosaic)> = vec![
            ("unpack (raw u16)", OutputFormat::RawU16, OutputFormat::GpuRawU16, Demosaic::Bilinear),
            ("RAW→RGBA8", OutputFormat::Rgba8, OutputFormat::GpuRgba8, Demosaic::Bilinear),
            ("RAW→RGBA8 half", OutputFormat::Rgba8, OutputFormat::GpuRgba8, Demosaic::HalfSize),
        ];
        for (case, host, resident, demosaic) in cases {
            if host == OutputFormat::RawU16 && !packed {
                continue; // nothing to compare: entropy decoding is CPU-only
            }
            let o = |backend, output| DecodeOptions { backend, output, demosaic, ..Default::default() };
            let cpu = bench(&raw, &o(Backend::CpuSimd, host), iters);
            let gpu = bench(&raw, &o(Backend::Gpu, host), iters);
            let res = bench(&raw, &o(Backend::Gpu, resident), iters);
            let auto = bench(&raw, &o(Backend::Auto, host), iters);
            let choice = raw
                .decode(o(Backend::Auto, host))
                .map(|f| if f.stats.backend.gpu_adapter.is_some() { "GPU" } else { "CPU" })
                .unwrap_or("-");
            let f = |s: &Option<Sample>| s.as_ref().map(|s| format!("{:.1}", s.wall)).unwrap_or("-".into());
            let st = gpu
                .as_ref()
                .map(|s| {
                    format!(
                        "{:.1} / {:.1} / {} / {:.1}",
                        s.upload,
                        s.compute,
                        s.kernel.map(|k| format!("{k:.1}")).unwrap_or("-".into()),
                        s.download
                    )
                })
                .unwrap_or_default();
            println!(
                "| {name} | {mp:.1} | {payload} | {case} | {} | {} | {} | {} ({choice}) | {st} | {} |",
                f(&cpu),
                f(&gpu),
                f(&res),
                f(&auto),
                gpu.as_ref().map(|s| s.bands).unwrap_or(0)
            );
        }
    }
}
