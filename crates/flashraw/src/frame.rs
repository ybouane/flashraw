//! Decoded frames and decode statistics.

use crate::options::OutputFormat;
use flashraw_core::*;

/// Pixel storage of a decoded frame.
#[derive(Debug, Clone, PartialEq)]
pub enum RawPixelData {
    U8(Vec<u8>),
    U16(Vec<u16>),
    F32(Vec<f32>),
    /// Data resident on the GPU (`OutputFormat::Gpu*`, see `flashraw::gpu`); download with `RawFrame::download`.
    #[cfg(feature = "gpu")]
    Gpu(std::sync::Arc<flashraw_gpu::GpuImage>),
}

impl RawPixelData {
    pub fn len(&self) -> usize {
        match self {
            RawPixelData::U8(v) => v.len(),
            RawPixelData::U16(v) => v.len(),
            RawPixelData::F32(v) => v.len(),
            #[cfg(feature = "gpu")]
            RawPixelData::Gpu(g) => g.len(),
        }
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn as_u16(&self) -> Option<&[u16]> {
        match self {
            RawPixelData::U16(v) => Some(v),
            _ => None,
        }
    }
    pub fn as_f32(&self) -> Option<&[f32]> {
        match self {
            RawPixelData::F32(v) => Some(v),
            _ => None,
        }
    }
    pub fn as_u8(&self) -> Option<&[u8]> {
        match self {
            RawPixelData::U8(v) => Some(v),
            _ => None,
        }
    }
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
pub struct BackendReport {
    /// e.g. "CPU AVX2 x16 threads"
    pub decode: String,
    /// Post-processing backend, if any ran.
    pub postprocess: Option<String>,
    pub simd: String,
    pub threads: usize,
    pub gpu_adapter: Option<String>,
}

/// Timing and resource statistics for one decode.
#[derive(Debug, Clone, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct DecodeStatistics {
    pub backend: BackendReport,
    /// Per-stage wall-clock milliseconds, in pipeline order.
    pub stages: Vec<(Stage, f64)>,
    pub total_ms: f64,
    /// Size of the input file.
    pub input_bytes: u64,
    /// Bytes of the output buffer.
    pub output_bytes: u64,
    /// DNG opcode lists: what was applied and what was not (empty without lists).
    pub opcodes: Vec<String>,
}

impl DecodeStatistics {
    pub fn stage_ms(&self, s: Stage) -> f64 {
        self.stages.iter().filter(|(x, _)| *x == s).map(|(_, v)| v).sum()
    }
}

/// A decoded frame: sensor-domain samples plus everything needed to interpret them.
#[derive(Debug, Clone)]
pub struct RawFrame {
    pub width: u32,
    pub height: u32,
    /// Samples per pixel (1 for CFA/mono, 3 for linear/RGB outputs, 4 for RGBA8).
    pub cpp: u32,
    /// Region of `data` holding valid image pixels.
    pub active_area: Rect,
    /// Recommended default crop.
    pub crop: Rect,
    /// Origin of this frame in sensor-buffer coordinates (non-zero for region decodes).
    pub origin: (u32, u32),
    pub data: RawPixelData,
    pub output: OutputFormat,
    pub bits_per_sample: u8,
    pub cfa: Option<CfaPattern>,
    pub sensor_layout: SensorLayout,
    pub masked_areas: Vec<Rect>,
    pub black_level: BlackLevel,
    pub white_level: WhiteLevel,
    pub white_balance: Option<WhiteBalance>,
    pub color_matrices: ColorMatrices,
    pub orientation: Orientation,
    pub metadata: RawMetadata,
    pub stats: DecodeStatistics,
}

impl RawFrame {
    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
    pub fn stats(&self) -> &DecodeStatistics {
        &self.stats
    }
    pub fn metadata(&self) -> &RawMetadata {
        &self.metadata
    }
    /// Sample slice for u16 outputs.
    pub fn as_u16(&self) -> Option<&[u16]> {
        self.data.as_u16()
    }
    pub fn as_f32(&self) -> Option<&[f32]> {
        self.data.as_f32()
    }
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
