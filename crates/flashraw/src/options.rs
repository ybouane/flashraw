//! Decode options and backend selection.

use flashraw_core::{Limits, Rect};

/// Execution backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
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
    /// Parse a backend name (`auto`, `cpu`/`scalar`, `simd`/`cpu-simd`, `gpu`/`webgpu`;
    /// case-insensitive).
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
#[non_exhaustive]
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
    // GPU-resident outputs (feature `gpu`): same content as the host variant, returned
    // as `RawPixelData::Gpu` without downloading (see `crate::gpu` and `RawFrame::download`).
    /// [`OutputFormat::RawU16`] kept on the GPU (u16 pairs per 32-bit word).
    GpuRawU16,
    /// [`OutputFormat::NormalizedF32`] kept on the GPU.
    GpuNormalizedF32,
    /// [`OutputFormat::LinearRgbF32`] kept on the GPU.
    GpuLinearRgbF32,
    /// [`OutputFormat::Rgba8`] kept on the GPU (one packed RGBA8 pixel per 32-bit word).
    GpuRgba8,
}

impl OutputFormat {
    /// True for the `Gpu*` variants (result stays on the device).
    pub fn is_gpu_resident(self) -> bool {
        matches!(
            self,
            OutputFormat::GpuRawU16
                | OutputFormat::GpuNormalizedF32
                | OutputFormat::GpuLinearRgbF32
                | OutputFormat::GpuRgba8
        )
    }
    /// The host-memory variant with the same content (identity for host variants).
    pub fn host_equivalent(self) -> Self {
        match self {
            OutputFormat::GpuRawU16 => OutputFormat::RawU16,
            OutputFormat::GpuNormalizedF32 => OutputFormat::NormalizedF32,
            OutputFormat::GpuLinearRgbF32 => OutputFormat::LinearRgbF32,
            OutputFormat::GpuRgba8 => OutputFormat::Rgba8,
            o => o,
        }
    }
}

impl OutputFormat {
    /// Parse an output name as accepted by the CLI and the JS bindings (`raw-u16`,
    /// `raw-f32`, `normalized`, `linear-rgb`, `rgba8`, `gpu-raw-u16`, `gpu-normalized`,
    /// `gpu-linear-rgb`, `gpu-rgba8`, plus a few aliases; case-insensitive, `_` or `-`).
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().replace('_', "-").as_str() {
            "raw" | "raw-u16" | "u16" => OutputFormat::RawU16,
            "raw-f32" | "f32" => OutputFormat::RawF32,
            "normalized" | "normalized-f32" => OutputFormat::NormalizedF32,
            "linear-rgb" | "linear-rgb-f32" | "rgb-f32" => OutputFormat::LinearRgbF32,
            "rgba8" | "rgba" | "imagedata" => OutputFormat::Rgba8,
            "gpu-raw-u16" | "gpu-raw" | "gpu-u16" => OutputFormat::GpuRawU16,
            "gpu-normalized" | "gpu-normalized-f32" => OutputFormat::GpuNormalizedF32,
            "gpu-linear-rgb" | "gpu-linear-rgb-f32" => OutputFormat::GpuLinearRgbF32,
            "gpu-rgba8" | "gpu-rgba" => OutputFormat::GpuRgba8,
            _ => return None,
        })
    }
}

/// Demosaic algorithm for RGB outputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
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
    /// Execution backend. Default: [`Backend::Auto`].
    pub backend: Backend,
    /// What the decode produces. Default: [`OutputFormat::RawU16`].
    pub output: OutputFormat,
    /// Worker threads (None = all cores; Some(1) = single-threaded).
    pub threads: Option<usize>,
    /// Decode only this region (sensor-buffer coordinates).
    pub region: Option<Rect>,
    /// Crop RGB outputs to the default crop (raw outputs are never cropped implicitly).
    pub crop: bool,
    /// Demosaic algorithm for RGB outputs. Default: [`Demosaic::Bilinear`].
    pub demosaic: Demosaic,
    /// Apply DNG opcode lists (bad pixels, lens-shading gain maps, ...) in their
    /// specified stages. Geometric opcodes are never applied. Default: true.
    pub apply_opcodes: bool,
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
            apply_opcodes: true,
            limits: Limits::default(),
        }
    }
}

impl Demosaic {
    /// Parse a demosaic name (`bilinear`, `half`/`half-size`; case-insensitive).
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().replace('_', "-").as_str() {
            "bilinear" => Demosaic::Bilinear,
            "half" | "half-size" | "halfsize" => Demosaic::HalfSize,
            _ => return None,
        })
    }

    /// Canonical name, accepted by [`Demosaic::parse`].
    pub fn name(self) -> &'static str {
        match self {
            Demosaic::Bilinear => "bilinear",
            Demosaic::HalfSize => "half-size",
        }
    }
}

impl Backend {
    /// Canonical name, accepted by [`Backend::parse`].
    pub fn name(self) -> &'static str {
        match self {
            Backend::Auto => "auto",
            Backend::Cpu => "cpu",
            Backend::CpuSimd => "simd",
            Backend::Gpu => "gpu",
        }
    }
}

impl OutputFormat {
    /// Canonical name, accepted by [`OutputFormat::parse`].
    pub fn name(self) -> &'static str {
        match self {
            OutputFormat::RawU16 => "raw-u16",
            OutputFormat::RawF32 => "raw-f32",
            OutputFormat::NormalizedF32 => "normalized",
            OutputFormat::LinearRgbF32 => "linear-rgb",
            OutputFormat::Rgba8 => "rgba8",
            OutputFormat::GpuRawU16 => "gpu-raw-u16",
            OutputFormat::GpuNormalizedF32 => "gpu-normalized",
            OutputFormat::GpuLinearRgbF32 => "gpu-linear-rgb",
            OutputFormat::GpuRgba8 => "gpu-rgba8",
        }
    }
}

/// Error returned when parsing an unknown option name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseOptionError {
    kind: &'static str,
    value: String,
}

impl std::fmt::Display for ParseOptionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown {} {:?}", self.kind, self.value)
    }
}

impl std::error::Error for ParseOptionError {}

macro_rules! name_traits {
    ($t:ty, $kind:literal) => {
        impl std::str::FromStr for $t {
            type Err = ParseOptionError;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                <$t>::parse(s).ok_or_else(|| ParseOptionError { kind: $kind, value: s.to_string() })
            }
        }

        impl std::fmt::Display for $t {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.name())
            }
        }
    };
}

name_traits!(Backend, "backend");
name_traits!(OutputFormat, "output format");
name_traits!(Demosaic, "demosaic");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        for b in [Backend::Auto, Backend::Cpu, Backend::CpuSimd, Backend::Gpu] {
            assert_eq!(b.to_string().parse::<Backend>(), Ok(b));
        }
        for o in [
            OutputFormat::RawU16,
            OutputFormat::RawF32,
            OutputFormat::NormalizedF32,
            OutputFormat::LinearRgbF32,
            OutputFormat::Rgba8,
            OutputFormat::GpuRawU16,
            OutputFormat::GpuNormalizedF32,
            OutputFormat::GpuLinearRgbF32,
            OutputFormat::GpuRgba8,
        ] {
            assert_eq!(o.to_string().parse::<OutputFormat>(), Ok(o));
        }
        for d in [Demosaic::Bilinear, Demosaic::HalfSize] {
            assert_eq!(d.to_string().parse::<Demosaic>(), Ok(d));
        }
        assert_eq!("nope".parse::<Backend>().unwrap_err().to_string(), "unknown backend \"nope\"");
    }
}
