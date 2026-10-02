//! CPU-vs-GPU backend selection.
//!
//! The decision compares *estimated* end-to-end times built from measured
//! throughputs: a one-time GPU micro-benchmark ([`GpuCalibration`], cached per
//! context) and CPU throughputs supplied by the caller ([`CpuCosts`], measured by
//! the `flashraw` facade the same way). Hard rules come first: software adapters are
//! never auto-selected, tiny frames stay on the CPU, and outputs that need no
//! processing never go through the GPU.

use crate::context::{AdapterClass, GpuContext};
use crate::run::{DevelopOutput, DevelopSpec, GpuFrame, GpuOp};
use flashraw_core::{CfaPattern, FrameInfo, Orientation, PackOrder, Rect, Result};
use std::sync::Arc;

/// Measured GPU characteristics.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GpuCalibration {
    /// Host → device bandwidth, bytes per ms.
    pub upload_bytes_per_ms: f64,
    /// Device → host bandwidth, bytes per ms.
    pub download_bytes_per_ms: f64,
    /// Fused develop (packed input → RGBA8), pixels per ms.
    pub develop_px_per_ms: f64,
    /// Unpack kernel, samples per ms.
    pub unpack_px_per_ms: f64,
    /// Fixed cost per run (submissions, synchronisation, allocations), ms.
    pub overhead_ms: f64,
    /// True when measured; false for the static defaults.
    pub measured: bool,
}

impl GpuCalibration {
    /// Conservative static defaults per adapter class (used when calibration is
    /// disabled with `FLASHRAW_GPU_CALIBRATION=static`).
    pub fn defaults(class: AdapterClass) -> Self {
        let (up, down, dev, unp, ovh) = match class {
            AdapterClass::Discrete => (10e6, 8e6, 4e6, 10e6, 1.0),
            AdapterClass::Integrated => (8e6, 8e6, 1e6, 3e6, 1.0),
            _ => (3e6, 3e6, 0.2e6, 0.5e6, 2.0),
        };
        GpuCalibration {
            upload_bytes_per_ms: up,
            download_bytes_per_ms: down,
            develop_px_per_ms: dev,
            unpack_px_per_ms: unp,
            overhead_ms: ovh,
            measured: false,
        }
    }
}

/// CPU throughputs (pixels per ms with all threads) for the same operations.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CpuCosts {
    /// Packed unpack (SIMD), samples per ms.
    pub unpack_px_per_ms: f64,
    /// Fused develop + RGBA8 encode (bilinear), output pixels per ms.
    pub develop_px_per_ms: f64,
    /// Normalisation, samples per ms.
    pub normalize_px_per_ms: f64,
}

/// What the caller wants produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkOutput {
    RawU16,
    Normalized,
    /// Developed RGB with this many output bytes per pixel (4 for RGBA8, 12 for f32 RGB).
    Rgb { bytes_per_px: u32, half_size: bool },
}

/// Description of one decode for the cost model.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Workload {
    /// Samples in the frame.
    pub samples: u64,
    /// Bytes uploaded on the GPU path (packed payload or u16 samples).
    pub upload_bytes: u64,
    /// The payload is bit-packed (GPU can unpack it; otherwise the CPU decodes first).
    pub packed: bool,
    pub output: WorkOutput,
    /// The result stays on the GPU (no download).
    pub resident: bool,
}

/// Outcome of [`decide`].
#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    pub use_gpu: bool,
    pub reason: String,
    pub est_cpu_ms: f64,
    pub est_gpu_ms: f64,
}

/// Frames below this size stay on the CPU (fixed GPU overheads dominate).
pub const MIN_GPU_SAMPLES: u64 = 2_000_000;
/// The GPU must win by this factor (estimates are noisy; CPU is the safer default).
pub const GPU_MARGIN: f64 = 0.85;

/// Estimate CPU and GPU times and choose.
pub fn decide(class: AdapterClass, gpu: &GpuCalibration, cpu: &CpuCosts, w: &Workload) -> Decision {
    let out_px = match w.output {
        WorkOutput::Rgb { half_size: true, .. } => w.samples / 4,
        _ => w.samples,
    };
    let (cpu_ms, gpu_kernel_ms, out_bytes) = match w.output {
        WorkOutput::RawU16 => {
            let c = if w.packed { w.samples as f64 / cpu.unpack_px_per_ms } else { 0.0 };
            (c, w.samples as f64 / gpu.unpack_px_per_ms, w.samples * 2)
        }
        WorkOutput::Normalized => (
            w.samples as f64 / cpu.normalize_px_per_ms + if w.packed { w.samples as f64 / cpu.unpack_px_per_ms } else { 0.0 },
            w.samples as f64 / gpu.unpack_px_per_ms,
            w.samples * 4,
        ),
        WorkOutput::Rgb { bytes_per_px, .. } => (
            out_px as f64 / cpu.develop_px_per_ms + if w.packed { w.samples as f64 / cpu.unpack_px_per_ms } else { 0.0 },
            w.samples as f64 / gpu.develop_px_per_ms,
            out_px * bytes_per_px as u64,
        ),
    };
    let download_ms = if w.resident { 0.0 } else { out_bytes as f64 / gpu.download_bytes_per_ms };
    let gpu_ms = gpu.overhead_ms + w.upload_bytes as f64 / gpu.upload_bytes_per_ms + gpu_kernel_ms + download_ms;
    let mk = |use_gpu: bool, reason: String| Decision { use_gpu, reason, est_cpu_ms: cpu_ms, est_gpu_ms: gpu_ms };
    if class == AdapterClass::Software {
        return mk(false, "software adapter".into());
    }
    if w.resident {
        return mk(true, "GPU-resident output requested".into());
    }
    if w.output == WorkOutput::RawU16 && !w.packed {
        return mk(false, "raw output of an entropy-coded payload needs no GPU work".into());
    }
    if w.samples < MIN_GPU_SAMPLES {
        return mk(false, format!("small frame ({:.1} MP)", w.samples as f64 / 1e6));
    }
    if gpu_ms < cpu_ms * GPU_MARGIN {
        mk(true, format!("estimated GPU {gpu_ms:.1} ms < CPU {cpu_ms:.1} ms"))
    } else {
        mk(false, format!("estimated CPU {cpu_ms:.1} ms <= GPU {gpu_ms:.1} ms"))
    }
}

fn calibration_disabled() -> bool {
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::env::var("FLASHRAW_GPU_CALIBRATION").map(|v| v.eq_ignore_ascii_case("static")).unwrap_or(false)
    }
    #[cfg(target_arch = "wasm32")]
    {
        false
    }
}

impl GpuContext {
    /// Measured characteristics of this device (one-time micro-benchmark, ~2048x2048
    /// synthetic frame; cached). Returns static defaults if the benchmark fails or
    /// `FLASHRAW_GPU_CALIBRATION=static`.
    pub async fn calibration(self: &Arc<Self>) -> GpuCalibration {
        if let Some(c) = self.calibration.get() {
            return *c;
        }
        let c = if calibration_disabled() {
            GpuCalibration::defaults(self.info.class)
        } else {
            self.measure().await.unwrap_or_else(|_| GpuCalibration::defaults(self.info.class))
        };
        *self.calibration.get_or_init(|| c)
    }

    /// Blocking [`GpuContext::calibration`] (native only).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn calibration_blocking(self: &Arc<Self>) -> GpuCalibration {
        pollster::block_on(self.calibration())
    }

    async fn measure(self: &Arc<Self>) -> Result<GpuCalibration> {
        const W: u32 = 2048;
        const H: u32 = 2048;
        let bits = 12u32;
        let stride = (W * bits / 8) as usize;
        let data: Vec<u8> = (0..stride * H as usize).map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8).collect();
        let payload = flashraw_core::PackedPayload {
            data: &data,
            width: W,
            height: H,
            bits,
            order: PackOrder::MsbBytes,
            row_stride: stride,
        };
        let frame = GpuFrame::from_packed(&payload);
        let mut info = FrameInfo::new(W, H, bits as u8);
        info.cfa = Some(CfaPattern::RGGB);
        let dev = GpuOp::Develop(DevelopSpec {
            crop: Rect::full(W, H),
            half_size: false,
            wb: [2.0, 1.0, 1.5, 1.0],
            cam_to_rgb: [[1.6, -0.4, -0.2], [-0.2, 1.4, -0.2], [0.0, -0.5, 1.5]],
            orientation: Orientation::Normal,
            output: DevelopOutput::Rgba8,
        });
        // Warm-up compiles the pipelines and faults in allocations.
        let warm = self.run(&frame, &info, &dev).await?;
        drop(warm);
        let mut best = (f64::MAX, f64::MAX, f64::MAX, f64::MAX);
        for _ in 0..2 {
            let r = self.run(&frame, &info, &dev).await?;
            let (_, dl) = r.image.download_async().await?;
            let k = r.timings.kernel_ms.unwrap_or(r.timings.compute_ms);
            best.0 = best.0.min(r.timings.upload_ms);
            best.1 = best.1.min(k);
            best.2 = best.2.min(dl);
            best.3 = best.3.min((r.timings.compute_ms - k).max(0.0));
        }
        let _ = self.run(&frame, &info, &GpuOp::Unpack).await?;
        let mut unpack = f64::MAX;
        for _ in 0..2 {
            let r = self.run(&frame, &info, &GpuOp::Unpack).await?;
            unpack = unpack.min(r.timings.kernel_ms.unwrap_or(r.timings.compute_ms));
        }
        let px = (W * H) as f64;
        Ok(GpuCalibration {
            upload_bytes_per_ms: data.len() as f64 / best.0.max(1e-3),
            download_bytes_per_ms: px * 4.0 / best.2.max(1e-3),
            develop_px_per_ms: px / best.1.max(1e-3),
            unpack_px_per_ms: px / unpack.max(1e-3),
            overhead_ms: best.3 + 0.2,
            measured: true,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cpu() -> CpuCosts {
        CpuCosts { unpack_px_per_ms: 2e6, develop_px_per_ms: 0.3e6, normalize_px_per_ms: 1e6 }
    }

    fn work(output: WorkOutput, packed: bool, mp: f64) -> Workload {
        let samples = (mp * 1e6) as u64;
        Workload { samples, upload_bytes: samples * 14 / 8, packed, output, resident: false }
    }

    #[test]
    fn software_never_auto() {
        let g = GpuCalibration::defaults(AdapterClass::Discrete);
        let d = decide(AdapterClass::Software, &g, &cpu(), &work(WorkOutput::Rgb { bytes_per_px: 4, half_size: false }, true, 45.0));
        assert!(!d.use_gpu);
    }

    #[test]
    fn small_frames_stay_on_cpu() {
        let g = GpuCalibration::defaults(AdapterClass::Discrete);
        let d = decide(AdapterClass::Discrete, &g, &cpu(), &work(WorkOutput::Rgb { bytes_per_px: 4, half_size: false }, true, 1.0));
        assert!(!d.use_gpu);
    }

    #[test]
    fn raw_entropy_coded_stays_on_cpu() {
        let g = GpuCalibration::defaults(AdapterClass::Discrete);
        let d = decide(AdapterClass::Discrete, &g, &cpu(), &work(WorkOutput::RawU16, false, 45.0));
        assert!(!d.use_gpu);
    }

    #[test]
    fn large_rgb_on_fast_gpu() {
        let g = GpuCalibration::defaults(AdapterClass::Discrete);
        let d = decide(AdapterClass::Discrete, &g, &cpu(), &work(WorkOutput::Rgb { bytes_per_px: 4, half_size: false }, true, 45.0));
        assert!(d.use_gpu, "{d:?}");
        // A much faster CPU flips the decision.
        let fast = CpuCosts { unpack_px_per_ms: 20e6, develop_px_per_ms: 10e6, normalize_px_per_ms: 20e6 };
        let d = decide(AdapterClass::Discrete, &g, &fast, &work(WorkOutput::Rgb { bytes_per_px: 4, half_size: false }, true, 45.0));
        assert!(!d.use_gpu, "{d:?}");
    }

    #[test]
    fn resident_output_forces_gpu() {
        let g = GpuCalibration::defaults(AdapterClass::Integrated);
        let mut w = work(WorkOutput::Rgb { bytes_per_px: 4, half_size: false }, true, 0.5);
        w.resident = true;
        assert!(decide(AdapterClass::Integrated, &g, &cpu(), &w).use_gpu);
    }
}
