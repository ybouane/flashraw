//! Decode options and backend selection.

use flashraw_core::{Limits, Rect};

/// Execution backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Backend {
    /// Pick the fastest available backend for this file and output.
    #[default]
    Auto,
    /// Portable scalar CPU code (reference implementations).
    Cpu,
    /// CPU with explicit SIMD kernels (runtime dispatched).
    CpuSimd,
    /// GPU compute (requires the `gpu` feature and an adapter).
    Gpu,
}

impl Backend {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().as_str() {
            "auto" => Backend::Auto,
            "cpu" | "scalar" => Backend::Cpu,
            "simd" | "cpu-simd" | "cpusimd" => Backend::CpuSimd,
            "gpu" | "webgpu" => Backend::Gpu,
            _ => return None,
        })
    }
}

/// What the decode produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum OutputFormat {
    /// Sensor samples as stored (u16, or f32 for floating-point DNG). No processing.
    #[default]
    RawU16,
    /// Sensor samples converted to f32 without normalisation.
    RawF32,
    /// Black-subtracted, white-normalised sensor samples in `[0, 1]` (f32, CFA layout kept).
    NormalizedF32,
    /// Demosaiced, white-balanced linear camera RGB → linear sRGB primaries, f32 RGB.
    LinearRgbF32,
    /// Display-ready 8-bit sRGB RGBA (orientation applied), e.g. for `ImageData`.
    Rgba8,
}

impl OutputFormat {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().replace('_', "-").as_str() {
            "raw" | "raw-u16" | "u16" => OutputFormat::RawU16,
            "raw-f32" | "f32" => OutputFormat::RawF32,
            "normalized" | "normalized-f32" => OutputFormat::NormalizedF32,
            "linear-rgb" | "linear-rgb-f32" | "rgb-f32" => OutputFormat::LinearRgbF32,
            "rgba8" | "rgba" | "imagedata" => OutputFormat::Rgba8,
            _ => return None,
        })
    }
}

/// Demosaic algorithm for RGB outputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Demosaic {
    /// Bilinear interpolation (Bayer), nearest-same-colour average (other CFAs).
    #[default]
    Bilinear,
    /// 2x2 superpixel binning: half resolution, fastest, no interpolation artefacts.
    HalfSize,
}

/// Options for [`crate::FlashRaw::decode`].
#[derive(Debug, Clone)]
pub struct DecodeOptions {
    pub backend: Backend,
    pub output: OutputFormat,
    /// Worker threads (None = all cores; Some(1) = single-threaded).
    pub threads: Option<usize>,
    /// Decode only this region (sensor-buffer coordinates).
    pub region: Option<Rect>,
    /// Crop RGB outputs to the default crop (raw outputs are never cropped implicitly).
    pub crop: bool,
    pub demosaic: Demosaic,
    /// Safety limits for untrusted input.
    pub limits: Limits,
}

impl Default for DecodeOptions {
    fn default() -> Self {
        DecodeOptions {
            backend: Backend::Auto,
            output: OutputFormat::RawU16,
            threads: None,
            region: None,
            crop: true,
            demosaic: Demosaic::Bilinear,
            limits: Limits::default(),
        }
    }
}
