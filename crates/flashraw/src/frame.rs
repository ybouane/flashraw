//! Decoded frames and decode statistics.

use crate::options::OutputFormat;
use flashraw_core::*;

/// Pixel storage of a decoded frame.
#[derive(Debug, Clone, PartialEq)]
pub enum RawPixelData {
    /// 8-bit samples ([`OutputFormat::Rgba8`]).
    U8(Vec<u8>),
    /// 16-bit samples ([`OutputFormat::RawU16`]).
    U16(Vec<u16>),
    /// `f32` samples ([`OutputFormat::RawF32`], [`OutputFormat::NormalizedF32`],
    /// [`OutputFormat::LinearRgbF32`]).
    F32(Vec<f32>),
    /// Data resident on the GPU (`OutputFormat::Gpu*`, see `flashraw::gpu`); download with `RawFrame::download`.
    #[cfg(feature = "gpu")]
    Gpu(std::sync::Arc<flashraw_gpu::GpuImage>),
}

impl RawPixelData {
    /// Number of samples (bytes for RGBA8 data).
    pub fn len(&self) -> usize {
        match self {
            RawPixelData::U8(v) => v.len(),
            RawPixelData::U16(v) => v.len(),
            RawPixelData::F32(v) => v.len(),
            #[cfg(feature = "gpu")]
            RawPixelData::Gpu(g) => g.len(),
        }
    }
    /// True if there are no samples.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// The samples if stored as `u16` in host memory; `None` otherwise.
    pub fn as_u16(&self) -> Option<&[u16]> {
        match self {
            RawPixelData::U16(v) => Some(v),
            _ => None,
        }
    }
    /// The samples if stored as `f32` in host memory; `None` otherwise.
    pub fn as_f32(&self) -> Option<&[f32]> {
        match self {
            RawPixelData::F32(v) => Some(v),
            _ => None,
        }
    }
    /// The samples if stored as `u8` in host memory; `None` otherwise.
    pub fn as_u8(&self) -> Option<&[u8]> {
        match self {
            RawPixelData::U8(v) => Some(v),
            _ => None,
        }
    }
    /// Size of the sample data in bytes.
    pub fn byte_len(&self) -> usize {
        match self {
            RawPixelData::U8(v) => v.len(),
            RawPixelData::U16(v) => v.len() * 2,
            RawPixelData::F32(v) => v.len() * 4,
            #[cfg(feature = "gpu")]
            RawPixelData::Gpu(g) => g.byte_len(),
        }
    }
}

/// Which backend actually ran each part of the decode.
#[derive(Debug, Clone, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct BackendReport {
    /// e.g. "CPU AVX2 x16 threads"
    pub decode: String,
    /// Post-processing backend, if any ran.
    pub postprocess: Option<String>,
    /// SIMD kernel level used (e.g. `"AVX2"`), or `"scalar"`.
    pub simd: String,
    /// Worker threads available to the decode.
    pub threads: usize,
    /// GPU adapter description when the GPU backend ran; `None` otherwise.
    pub gpu_adapter: Option<String>,
}

/// Timing and resource statistics for one decode.
#[derive(Debug, Clone, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct DecodeStatistics {
    /// Which backends ran.
    pub backend: BackendReport,
    /// Per-stage wall-clock milliseconds, in pipeline order.
    pub stages: Vec<(Stage, f64)>,
    /// Total wall-clock milliseconds, including container parsing in `open`.
    pub total_ms: f64,
    /// Size of the input file.
    pub input_bytes: u64,
    /// Bytes of the output buffer.
    pub output_bytes: u64,
    /// DNG opcode lists: what was applied and what was not (empty without lists).
    pub opcodes: Vec<String>,
}

impl DecodeStatistics {
    /// Milliseconds recorded for stage `s` (summed over entries; 0 if it did not run).
    pub fn stage_ms(&self, s: Stage) -> f64 {
        self.stages.iter().filter(|(x, _)| *x == s).map(|(_, v)| v).sum()
    }
}

/// A decoded frame: sensor-domain samples plus everything needed to interpret them.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RawFrame {
    /// Width of `data` in pixels: the region width for region decodes, the crop width
    /// for cropped RGB outputs, and after orientation for [`OutputFormat::Rgba8`].
    pub width: u32,
    /// Height of `data` in pixels (see `width`).
    pub height: u32,
    /// Samples per pixel (1 for CFA/mono, 3 for linear/RGB outputs, 4 for RGBA8).
    pub cpp: u32,
    /// Region of `data` holding valid image pixels.
    pub active_area: Rect,
    /// Recommended default crop.
    pub crop: Rect,
    /// Origin of this frame in sensor-buffer coordinates (non-zero for region decodes).
    pub origin: (u32, u32),
    /// The samples, row-major with `cpp` interleaved samples per pixel.
    pub data: RawPixelData,
    /// The output format this frame was decoded to.
    pub output: OutputFormat,
    /// Significant bits per sample of the sensor data as stored in the file (e.g. 12 or 14).
    pub bits_per_sample: u8,
    /// CFA pattern anchored at sensor-buffer `(0, 0)` (use [`RawFrame::color_at`] for frame
    /// coordinates); `None` for RGB outputs and sensors without a CFA.
    pub cfa: Option<CfaPattern>,
    /// Sample layout; [`SensorLayout::Linear`] for RGB outputs.
    pub sensor_layout: SensorLayout,
    /// Masked (optical black) areas inside this frame, in frame coordinates; empty for RGB
    /// outputs.
    pub masked_areas: Vec<Rect>,
    /// Black level in raw sample units, anchored at sensor-buffer `(0, 0)` (measured from the
    /// masked areas when the format requires it). Scaled with the samples when floating-point
    /// data is converted for [`OutputFormat::RawU16`].
    pub black_level: BlackLevel,
    /// White (saturation) level in raw sample units; 16383 when floating-point data was
    /// scaled for [`OutputFormat::RawU16`].
    pub white_level: WhiteLevel,
    /// As-shot white balance; `None` when the file records none.
    pub white_balance: Option<WhiteBalance>,
    /// Colour calibration of the camera.
    pub color_matrices: ColorMatrices,
    /// Orientation still to apply for display; [`Orientation::Normal`] for
    /// [`OutputFormat::Rgba8`], which already has it applied.
    pub orientation: Orientation,
    /// Normalised file metadata (as returned by [`crate::FlashRaw::metadata`]).
    pub metadata: RawMetadata,
    /// Timings and backends of this decode.
    pub stats: DecodeStatistics,
    /// Repairs made under [`crate::CorruptionPolicy::Recover`] (truncated strips/tiles
    /// zero-filled, ...), in unit order. Always empty under the default strict policy.
    pub warnings: Vec<DecodeWarning>,
}

impl RawFrame {
    /// Width of the frame in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }
    /// Height of the frame in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }
    /// Timings and backends of this decode.
    pub fn stats(&self) -> &DecodeStatistics {
        &self.stats
    }
    /// Repairs made under [`crate::CorruptionPolicy::Recover`]; empty for a complete decode.
    pub fn warnings(&self) -> &[DecodeWarning] {
        &self.warnings
    }
    /// True when some samples hold no decoded data (recovered from corrupt or truncated
    /// input, see [`RawFrame::warnings`]).
    pub fn is_incomplete(&self) -> bool {
        !self.warnings.is_empty()
    }
    /// Normalised file metadata.
    pub fn metadata(&self) -> &RawMetadata {
        &self.metadata
    }
    /// Sample slice for u16 outputs.
    pub fn as_u16(&self) -> Option<&[u16]> {
        self.data.as_u16()
    }
    /// Sample slice for f32 outputs.
    pub fn as_f32(&self) -> Option<&[f32]> {
        self.data.as_f32()
    }
    /// Sample slice for 8-bit outputs ([`OutputFormat::Rgba8`]).
    pub fn as_u8(&self) -> Option<&[u8]> {
        self.data.as_u8()
    }
    /// CFA colour at frame coordinates (accounts for region origin).
    pub fn color_at(&self, x: u32, y: u32) -> Option<CfaColor> {
        self.cfa.map(|p| p.color_at((x + self.origin.0) as usize, (y + self.origin.1) as usize))
    }
    /// FNV-1a 64 hash over the little-endian bytes of u16 samples in `rect` (all
    /// components). Reported by `flashraw verify` for regression testing.
    pub fn hash_u16(&self, rect: Rect) -> Option<u64> {
        let d = self.as_u16()?;
        let cpp = self.cpp as usize;
        const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let r = rect.clamp_to(self.width, self.height);
        for y in r.y as usize..r.bottom() as usize {
            let row = &d
                [(y * self.width as usize + r.x as usize) * cpp..(y * self.width as usize + r.right() as usize) * cpp];
            for &v in row {
                h ^= (v & 0xff) as u64;
                h = h.wrapping_mul(FNV_PRIME);
                h ^= (v >> 8) as u64;
                h = h.wrapping_mul(FNV_PRIME);
            }
        }
        Some(h)
    }
}
