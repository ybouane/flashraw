//! GPU backend integration (feature `gpu`): backend selection for [`Backend::Auto`],
//! the GPU decode path for [`Backend::Gpu`], GPU-resident outputs
//! (`OutputFormat::Gpu*` → [`RawPixelData::Gpu`]) and downloads.
//!
//! Data path: if the format exposes a bit-packed payload
//! ([`crate::Capabilities::gpu_unpack`]) the *file bytes* are uploaded and unpacked on
//! the GPU inside the fused kernel; otherwise the CPU decodes (entropy decoding is
//! sequential) and the u16/f32 samples are uploaded once. Post-processing runs on the
//! GPU and the result is downloaded or left on the device. See `docs/gpu.md`.

use crate::options::{Backend, DecodeOptions, OutputFormat};
use crate::{FlashRaw, RawFrame, RawPixelData, process};
use flashraw_core::{
    Error, FrameInfo, Orientation, PackedPayload, PixelData, Profiler, RawImage, Rect, Result, SampleFormat,
    SensorLayout, Stage,
};
use flashraw_gpu::policy::{self, CpuCosts, Decision, WorkOutput, Workload};
use flashraw_gpu::{DevelopOutput, DevelopSpec, GpuFrame, GpuOp, HostPixels};
use std::sync::{Arc, Mutex, OnceLock};

pub use flashraw_gpu::policy::GpuCalibration;
pub use flashraw_gpu::{
    AdapterClass, GpuAdapterInfo, GpuContext, GpuImage, GpuPixelFormat, GpuTile, adapter_info, init, set_context,
    try_context, wgpu,
};

/// The global GPU context (initialised on first use on native; see [`init`] for the browser).
pub fn context() -> Result<Arc<GpuContext>> {
    flashraw_gpu::context()
}

/// Run the one-time GPU and CPU calibrations used by [`Backend::Auto`] now (e.g. at
/// application start-up) instead of on the first large decode.
#[cfg(not(target_arch = "wasm32"))]
pub fn calibrate() -> Result<(GpuCalibration, CpuCosts)> {
    let ctx = flashraw_gpu::context()?;
    Ok((ctx.calibration_blocking(), cpu_costs()))
}

/// What [`Backend::Auto`] would choose for this file and options (`None` when the
/// decision does not involve the GPU at all, e.g. raw outputs or tiny frames).
pub fn auto_decision(raw: &FlashRaw<'_>, opts: &DecodeOptions) -> Option<Decision> {
    let mut w = workload(raw, opts)?;
    let ctx = flashraw_gpu::context().ok()?;
    // Frames beyond one storage binding run tiled; the cost model charges the bands.
    w.bands = ctx.estimate_bands(w.upload_bytes, w.output_bytes());
    #[cfg(not(target_arch = "wasm32"))]
    let g = ctx.calibration_blocking();
    #[cfg(target_arch = "wasm32")]
    let g = ctx.cached_calibration().unwrap_or_else(|| GpuCalibration::defaults(ctx.info().class));
    Some(policy::decide(ctx.info().class, &g, &cpu_costs(), &w))
}

/// Packed payload usable by the GPU path (same geometry as the frame, integer CFA/mono
/// samples, no black level measured from masked pixels).
fn gpu_packed<'a>(raw: &'a FlashRaw<'_>) -> Option<PackedPayload<'a>> {
    let info = raw.decoder.frame_info();
    raw.decoder.packed_payload().filter(|p| {
        !info.black_from_masked
            && info.cpp == 1
            && info.sample_format == SampleFormat::U16
            && p.width == info.width
            && p.height == info.height
            && p.is_complete()
    })
}

fn workload(raw: &FlashRaw<'_>, opts: &DecodeOptions) -> Option<Workload> {
    let info = raw.decoder.frame_info();
    let full = Rect::full(info.width, info.height);
    let win = opts.region.and_then(|r| r.intersect(&full)).unwrap_or(full);
    let samples = win.width as u64 * win.height as u64 * info.cpp as u64;
    let packed = gpu_packed(raw);
    let half_size = opts.demosaic == crate::Demosaic::HalfSize;
    let output = match opts.output.host_equivalent() {
        OutputFormat::RawU16 => WorkOutput::RawU16,
        OutputFormat::NormalizedF32 => WorkOutput::Normalized,
        OutputFormat::LinearRgbF32 => WorkOutput::Rgb { bytes_per_px: 12, half_size },
        OutputFormat::Rgba8 => WorkOutput::Rgb { bytes_per_px: 4, half_size },
        _ => return None,
    };
    let bytes_per_sample = if info.sample_format == SampleFormat::F32 { 4 } else { 2 };
    Some(Workload {
        samples,
        upload_bytes: match packed {
            Some(p) => (p.data.len() as u64).min(p.row_stride as u64 * win.bottom() as u64),
            None => samples * bytes_per_sample,
        },
        packed: packed.is_some(),
        output,
        resident: opts.output.is_gpu_resident(),
        bands: 1,
    })
}

/// Decide whether this decode runs on the GPU. `Ok(None)` = CPU path.
fn select(raw: &FlashRaw<'_>, opts: &DecodeOptions) -> Result<Option<(Arc<GpuContext>, Option<String>)>> {
    let resident = opts.output.is_gpu_resident();
    let base = opts.output.host_equivalent();
    // DNG opcode lists that change this output are applied by the CPU pipeline.
    if !resident
        && opts.apply_opcodes
        && crate::opcodes::Lists::of(raw.frame_info()).is_some_and(|l| l.affects(base))
        && !matches!(opts.backend, Backend::Cpu | Backend::CpuSimd)
    {
        return Ok(None);
    }
    match opts.backend {
        Backend::Cpu | Backend::CpuSimd => {
            if resident {
                return Err(Error::InvalidArgument(format!(
                    "{:?} is a GPU-resident output; use Backend::Gpu or Backend::Auto",
                    opts.output
                )));
            }
            Ok(None)
        }
        Backend::Gpu => {
            // Raw output of an entropy-coded file: the CPU decode *is* the result.
            if base == OutputFormat::RawF32 || (base == OutputFormat::RawU16 && !resident && gpu_packed(raw).is_none())
            {
                return Ok(None);
            }
            Ok(Some((flashraw_gpu::context()?, None)))
        }
        Backend::Auto => {
            if resident {
                return Ok(Some((flashraw_gpu::context()?, Some("GPU-resident output".into()))));
            }
            // Raw outputs never touch the GPU under Auto (transfer-bound, see docs/gpu.md):
            // this also keeps the default `decode()` free of any GPU initialisation.
            if matches!(base, OutputFormat::RawU16 | OutputFormat::RawF32) || cfg!(target_arch = "wasm32") {
                return Ok(None);
            }
            let Some(w) = workload(raw, opts) else { return Ok(None) };
            if w.samples < policy::MIN_GPU_SAMPLES {
                return Ok(None);
            }
            let Ok(ctx) = flashraw_gpu::context() else { return Ok(None) };
            if ctx.info().class == AdapterClass::Software {
                return Ok(None);
            }
            match auto_decision(raw, opts) {
                Some(d) if d.use_gpu => Ok(Some((ctx, Some(format!("auto: {}", d.reason))))),
                _ => Ok(None),
            }
        }
    }
}

/// Hook called by `FlashRaw::decode_inner`: runs the decode on the GPU when selected.
pub(crate) fn decode(
    raw: &FlashRaw<'_>,
    opts: &DecodeOptions,
    t0: f64,
    profiler: &Profiler,
) -> Result<Option<RawFrame>> {
    let Some((ctx, why)) = select(raw, opts)? else { return Ok(None) };
    #[cfg(not(target_arch = "wasm32"))]
    {
        flashraw_gpu::block_on(run(raw, opts, t0, profiler, ctx, why)).map(Some)
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (ctx, why, t0, profiler);
        Err(Error::GpuUnavailable(
            "synchronous GPU decoding is unavailable in the browser; use decode_gpu_async".into(),
        ))
    }
}

/// The GPU pipeline only runs when no opcode list changes the output (see [`select`]),
/// or for GPU-resident outputs, which do not apply them.
fn gpu_opcode_notes(raw: &FlashRaw<'_>) -> Vec<String> {
    let Some(mut l) = crate::opcodes::Lists::of(raw.frame_info()) else { return Vec::new() };
    (0..3).for_each(|i| l.record_unused(i, "GPU pipeline"));
    l.into_notes()
}

impl FlashRaw<'_> {
    /// Asynchronous GPU decode (required in the browser, where the GPU cannot be
    /// waited on synchronously; also usable on native). Uses the GPU path (call
    /// `flashraw::gpu::init().await` first in the browser), except for host outputs that a
    /// DNG opcode list changes: those are decoded by the CPU pipeline, like [`FlashRaw::decode`].
    pub async fn decode_gpu_async(&self, opts: DecodeOptions) -> Result<RawFrame> {
        if !opts.output.is_gpu_resident()
            && opts.apply_opcodes
            && crate::opcodes::Lists::of(self.frame_info()).is_some_and(|l| l.affects(opts.output))
        {
            return self.decode(DecodeOptions { backend: Backend::Auto, ..opts });
        }
        let t0 = flashraw_core::profile::now_ms();
        let profiler = Profiler::new();
        profiler.record(Stage::ContainerParse, self.parse_ms);
        let ctx = match flashraw_gpu::try_context() {
            Some(c) => c,
            None => flashraw_gpu::init().await?,
        };
        run(self, &opts, t0, &profiler, ctx, None).await
    }
}

fn host_data(px: HostPixels) -> RawPixelData {
    match px {
        HostPixels::U8(v) => RawPixelData::U8(v),
        HostPixels::U16(v) => RawPixelData::U16(v),
        HostPixels::F32(v) => RawPixelData::F32(v),
    }
}

async fn run(
    raw: &FlashRaw<'_>,
    opts: &DecodeOptions,
    t0: f64,
    profiler: &Profiler,
    ctx: Arc<GpuContext>,
    why: Option<String>,
) -> Result<RawFrame> {
    let info = raw.decoder.frame_info();
    let base = opts.output.host_equivalent();
    let resident = opts.output.is_gpu_resident();
    let full = Rect::full(info.width, info.height);
    let win = match opts.region {
        Some(r) => r.intersect(&full).ok_or_else(|| Error::InvalidArgument(format!("region {r} outside {full}")))?,
        None => full,
    };
    let threads = flashraw_core::par::threads();
    let mut black = info.black_level.clone();
    let packed = gpu_packed(raw);
    let img: Option<RawImage>;
    let dctx = crate::decode_context(opts, profiler, threads > 1, cfg!(feature = "simd"));
    let (frame, decode_backend) = match &packed {
        Some(p) => (GpuFrame::from_packed(p).with_window(win), format!("GPU unpack ({})", ctx.info().name)),
        None => {
            let (decoded, measured) = raw.decode_window(&dctx, win, profiler)?;
            if let Some(b) = measured {
                black = b;
            }
            img = Some(decoded);
            let d = img.as_ref().expect("just set");
            let f = match &d.data {
                PixelData::U16(v) => GpuFrame::from_u16(v, d.width, d.height, d.cpp),
                PixelData::F32(v) => {
                    if base == OutputFormat::RawU16 {
                        return Err(Error::InvalidArgument(
                            "GpuRawU16 of floating-point data is not supported; use GpuNormalizedF32".into(),
                        ));
                    }
                    GpuFrame::from_f32(v, d.width, d.height, d.cpp)
                }
            };
            let simd = if cfg!(feature = "simd") { flashraw_core::simd::detect().name() } else { "scalar" };
            (
                f.with_origin((win.x, win.y)),
                format!("CPU {simd} x{threads} thread{}", if threads == 1 { "" } else { "s" }),
            )
        }
    };
    let (fw, fh) = (frame.window.width, frame.window.height);
    let origin = (win.x, win.y);
    let frame_rect = Rect::new(origin.0, origin.1, fw, fh);
    let translate = |r: Rect| -> Rect {
        r.intersect(&frame_rect)
            .map(|i| Rect::new(i.x - origin.0, i.y - origin.1, i.width, i.height))
            .unwrap_or_default()
    };
    let mut post_info: FrameInfo = info.clone();
    post_info.black_level = black.clone();
    let op = match base {
        OutputFormat::RawU16 => GpuOp::Unpack,
        OutputFormat::NormalizedF32 => GpuOp::Normalize,
        OutputFormat::LinearRgbF32 | OutputFormat::Rgba8 => {
            let crop = if opts.crop { translate(info.crop) } else { Rect::full(fw, fh) };
            let crop = if crop.is_empty() { Rect::full(fw, fh) } else { crop };
            let params = process::DevelopParams::new(&post_info, opts.demosaic, crop);
            GpuOp::Develop(DevelopSpec {
                crop,
                half_size: opts.demosaic == crate::Demosaic::HalfSize,
                wb: params.wb,
                cam_to_rgb: params.cam_to_rgb,
                orientation: params.orientation,
                output: if base == OutputFormat::Rgba8 { DevelopOutput::Rgba8 } else { DevelopOutput::LinearRgbF32 },
            })
        }
        o => return Err(Error::InvalidArgument(format!("{o:?} is not produced by the GPU backend"))),
    };
    // Frames beyond the adapter's buffer limits are processed in bands (tiled).
    let r = ctx.run(&frame, &post_info, &op).await?;
    profiler.record(Stage::GpuUpload, r.timings.upload_ms);
    profiler.record(Stage::GpuCompute, r.timings.compute_ms);
    let (width, height, cpp) = (r.image.width(), r.image.height(), r.image.channels());
    // The Arc only makes frames cheap to clone. On wasm32 WebGPU handles are not Send, but
    // the GPU build is single-threaded there, so nothing crosses threads.
    #[allow(clippy::arc_with_non_send_sync)]
    let data = if resident {
        RawPixelData::Gpu(Arc::new(r.image))
    } else {
        let (px, ms) = r.image.download_async().await?;
        profiler.record(Stage::GpuDownload, ms);
        host_data(px)
    };
    let adapter = ctx.info().to_string();
    let mut post = format!("GPU {adapter}");
    if let Some(k) = r.timings.kernel_ms {
        post.push_str(&format!(", kernel {k:.2} ms"));
    }
    if r.timings.bands > 1 {
        post.push_str(&format!(", {} bands", r.timings.bands));
    }
    if let Some(w) = why {
        post.push_str(&format!(" [{w}]"));
    }
    let raw_output = matches!(base, OutputFormat::RawU16 | OutputFormat::NormalizedF32);
    let stats = crate::DecodeStatistics {
        backend: crate::BackendReport {
            decode: decode_backend,
            postprocess: (base != OutputFormat::RawU16 || packed.is_some()).then_some(post),
            simd: if cfg!(feature = "simd") {
                flashraw_core::simd::detect().name().to_string()
            } else {
                "scalar".into()
            },
            threads,
            gpu_adapter: Some(adapter),
        },
        stages: profiler.stages(),
        total_ms: flashraw_core::profile::now_ms() - t0 + raw.parse_ms,
        input_bytes: raw.bytes().len() as u64,
        output_bytes: data.byte_len() as u64,
        opcodes: gpu_opcode_notes(raw),
    };
    Ok(RawFrame {
        width,
        height,
        cpp,
        active_area: if raw_output { translate(info.active_area) } else { Rect::full(width, height) },
        crop: if raw_output { translate(info.crop) } else { Rect::full(width, height) },
        origin,
        data,
        output: opts.output,
        bits_per_sample: info.bits_per_sample,
        cfa: if raw_output { info.cfa } else { None },
        sensor_layout: if raw_output { info.layout } else { SensorLayout::Linear },
        masked_areas: if raw_output {
            info.masked_areas.iter().map(|r| translate(*r)).filter(|r| !r.is_empty()).collect()
        } else {
            vec![]
        },
        black_level: black,
        white_level: info.white_level.clone(),
        white_balance: info.white_balance,
        color_matrices: info.color_matrices.clone(),
        orientation: if base == OutputFormat::Rgba8 { Orientation::Normal } else { info.orientation },
        metadata: raw.metadata().clone(),
        stats,
        warnings: dctx.take_warnings(),
        source: crate::FrameSource::Sensor,
    })
}

impl RawFrame {
    /// The GPU-resident image, for `OutputFormat::Gpu*` outputs.
    pub fn as_gpu(&self) -> Option<&Arc<GpuImage>> {
        match &self.data {
            RawPixelData::Gpu(g) => Some(g),
            _ => None,
        }
    }

    /// Copy of this frame with GPU-resident data downloaded to host memory (output
    /// format switched to the host equivalent). Host frames are returned as-is.
    pub async fn download_async(&self) -> Result<RawFrame> {
        let RawPixelData::Gpu(g) = &self.data else { return Ok(self.clone()) };
        let (px, ms) = g.download_async().await?;
        let mut f = self.clone();
        f.data = host_data(px);
        f.output = self.output.host_equivalent();
        f.stats.stages.push((Stage::GpuDownload, ms));
        f.stats.total_ms += ms;
        f.stats.output_bytes = f.data.byte_len() as u64;
        Ok(f)
    }

    /// Blocking [`RawFrame::download_async`] (native only).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn download(&self) -> Result<RawFrame> {
        flashraw_gpu::block_on(self.download_async())
    }
}

/// Measured CPU throughputs (same kernels as the CPU decode path), cached per thread count.
pub fn cpu_costs() -> CpuCosts {
    static CACHE: OnceLock<Mutex<Vec<(usize, CpuCosts)>>> = OnceLock::new();
    let threads = flashraw_core::par::threads();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(c) = cache.lock().ok().and_then(|c| c.iter().find(|(t, _)| *t == threads).map(|(_, c)| *c)) {
        return c;
    }
    let c = measure_cpu();
    if let Ok(mut v) = cache.lock() {
        v.push((threads, c));
    }
    c
}

fn measure_cpu() -> CpuCosts {
    use flashraw_codecs::packed::{PackedLayout, unpack};
    const W: usize = 2048;
    const H: usize = 1024;
    let parallel = flashraw_core::par::threads() > 1;
    let layout = PackedLayout::tight(W, H, 12, flashraw_core::PackOrder::MsbBytes);
    let src: Vec<u8> = (0..layout.row_stride * H).map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8).collect();
    let best = |f: &mut dyn FnMut()| {
        let mut b = f64::MAX;
        for _ in 0..3 {
            let t = flashraw_core::profile::now_ms();
            f();
            b = b.min(flashraw_core::profile::now_ms() - t);
        }
        b.max(1e-3)
    };
    let mut img = match RawImage::new_u16(W as u32, H as u32, 1, &flashraw_core::Limits::default()) {
        Ok(i) => i,
        Err(_) => {
            return CpuCosts {
                unpack_px_per_ms: 1e6,
                develop_px_per_ms: 1e5,
                develop_half_px_per_ms: 2e5,
                normalize_px_per_ms: 5e5,
            };
        }
    };
    let t_unpack = best(&mut || {
        let _ = unpack(&src, layout, img.u16_mut(), W, parallel, cfg!(feature = "simd"), true);
    });
    let mut info = FrameInfo::new(W as u32, H as u32, 12);
    info.cfa = Some(flashraw_core::CfaPattern::RGGB);
    info.black_level = flashraw_core::BlackLevel::uniform(64.0);
    let params = process::DevelopParams::new(&info, crate::Demosaic::Bilinear, Rect::full(W as u32, H as u32));
    let t_dev = best(&mut || {
        let _ = process::develop_rgba8(&img, &info, (0, 0), &params, parallel);
    });
    let half = process::DevelopParams::new(&info, crate::Demosaic::HalfSize, Rect::full(W as u32, H as u32));
    let t_half = best(&mut || {
        let _ = process::develop_rgba8(&img, &info, (0, 0), &half, parallel);
    });
    let t_norm = best(&mut || {
        let _ = process::normalize(&img, &info, (0, 0), parallel);
    });
    let px = (W * H) as f64;
    CpuCosts {
        unpack_px_per_ms: px / t_unpack,
        develop_px_per_ms: px / t_dev,
        develop_half_px_per_ms: px / t_half,
        normalize_px_per_ms: px / t_norm,
    }
}
