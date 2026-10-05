//! The contract between container/format decoders and the high-level API.

use crate::Result;
use crate::image::{Limits, RawImage};
use crate::metadata::RawMetadata;
use crate::profile::Profiler;
use crate::types::*;

/// Numeric format of decoded samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub enum SampleFormat {
    /// Unsigned integer samples (`u16`).
    U16,
    /// Floating-point samples (`f32`, e.g. floating-point DNG).
    F32,
}

/// Geometry and calibration of the decoded sensor buffer.
///
/// Everything here is known after parsing, before any pixel is decoded.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct FrameInfo {
    /// Full decoded buffer width (including masked areas).
    pub width: u32,
    /// Full decoded buffer height (including masked areas).
    pub height: u32,
    /// Samples per pixel.
    pub cpp: u32,
    /// Significant bits per sample (e.g. 12 or 14).
    pub bits_per_sample: u8,
    /// Numeric format of the decoded samples.
    pub sample_format: SampleFormat,
    /// Colour filter pattern anchored at buffer `(0, 0)`; `None` for monochrome and
    /// multi-sample (linear, Foveon) data.
    pub cfa: Option<CfaPattern>,
    /// How samples are arranged on the sensor.
    pub layout: SensorLayout,
    /// Region of valid image pixels (excludes optical black / masked borders).
    pub active_area: Rect,
    /// Recommended default crop (DNG DefaultCrop or vendor equivalent); inside `active_area`.
    pub crop: Rect,
    /// Masked (optical black) regions usable for black-level estimation.
    pub masked_areas: Vec<Rect>,
    /// Black level (see also `black_from_masked`).
    pub black_level: BlackLevel,
    /// When true, `black_level` is a fallback: the actual per-CFA-colour black is measured
    /// from `masked_areas` after decoding (done by the high-level API).
    pub black_from_masked: bool,
    /// Saturation level of the stored samples.
    pub white_level: WhiteLevel,
    /// As-shot white balance; `None` when the file records none.
    pub white_balance: Option<WhiteBalance>,
    /// Colour calibration (file or camera database).
    pub color_matrices: ColorMatrices,
    /// Orientation to apply for display.
    pub orientation: Orientation,
    /// Pixel aspect ratio (width / height of a sensor site); 1.0 for almost all cameras.
    pub pixel_aspect: f32,
    /// Payload compression scheme.
    pub compression: Compression,
    /// DNG opcode lists carried by the file (applied by the high-level API).
    pub opcodes: Option<std::sync::Arc<OpcodeLists>>,
}

/// DNG opcode lists exactly as stored (big-endian opcode data, DNG spec chapter 7).
/// List 1 applies to stored values, list 2 to linear reference values of the active
/// area, list 3 to the demosaiced camera-RGB image.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct OpcodeLists {
    /// OpcodeList1, OpcodeList2 and OpcodeList3 (empty when absent).
    pub lists: [Vec<u8>; 3],
}

impl OpcodeLists {
    /// True if every list is absent or holds no opcodes (at most its 4-byte count).
    pub fn is_empty(&self) -> bool {
        self.lists.iter().all(|l| l.len() <= 4)
    }
}

impl FrameInfo {
    /// A default description for a `width x height` CFA buffer; formats fill in the rest.
    pub fn new(width: u32, height: u32, bits: u8) -> Self {
        FrameInfo {
            width,
            height,
            cpp: 1,
            bits_per_sample: bits,
            sample_format: SampleFormat::U16,
            cfa: None,
            layout: SensorLayout::Bayer,
            active_area: Rect::full(width, height),
            crop: Rect::full(width, height),
            masked_areas: Vec::new(),
            black_level: BlackLevel::uniform(0.0),
            black_from_masked: false,
            white_level: WhiteLevel::uniform(if bits >= 16 { 65535 } else { (1u32 << bits) - 1 }),
            white_balance: None,
            color_matrices: ColorMatrices::default(),
            orientation: Orientation::Normal,
            pixel_aspect: 1.0,
            compression: Compression::Uncompressed,
            opcodes: None,
        }
    }

    /// Clamp `active_area`/`crop`/masked areas into the buffer so later stages can trust them.
    pub fn sanitize(&mut self) {
        self.active_area = self.active_area.clamp_to(self.width, self.height);
        if self.active_area.is_empty() {
            self.active_area = Rect::full(self.width, self.height);
        }
        self.crop = self.crop.intersect(&self.active_area).unwrap_or(self.active_area);
        let (w, h) = (self.width, self.height);
        self.masked_areas.retain_mut(|r| {
            *r = r.clamp_to(w, h);
            !r.is_empty()
        });
        if self.white_level.values.is_empty() {
            self.white_level = WhiteLevel::uniform((1u32 << self.bits_per_sample.min(16)) - 1);
        }
    }
}

/// Context passed to decoders.
pub struct DecodeContext<'p> {
    /// Safety limits for allocations and dimensions.
    pub limits: Limits,
    /// Allow multithreaded decoding.
    pub parallel: bool,
    /// Allow explicit SIMD kernels (scalar reference paths otherwise).
    pub simd: bool,
    /// Receives per-stage timings.
    pub profiler: &'p Profiler,
}

impl<'p> DecodeContext<'p> {
    /// A context with default limits, parallelism and SIMD enabled.
    pub fn new(profiler: &'p Profiler) -> Self {
        DecodeContext { limits: Limits::default(), parallel: true, simd: true, profiler }
    }
}

/// How the payload can be accessed independently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub enum RandomAccess {
    /// A single sequential stream; any region requires decoding from the start.
    Sequential,
    /// Independent rows (e.g. uncompressed / packed data): any region is directly addressable.
    Rows,
    /// Independent strips of `rows` rows.
    Strips {
        /// Rows per strip (the last strip may be shorter).
        rows: u32,
    },
    /// Independent tiles.
    Tiles {
        /// Tile width in pixels.
        width: u32,
        /// Tile height in pixels.
        height: u32,
    },
    /// Independent vertical slices/blocks of varying geometry.
    Blocks,
}

/// An independently decodable unit of the payload.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DecodeUnit {
    /// Area of the output buffer covered by this unit.
    pub rect: Rect,
    /// Byte range of the compressed data in the source file (for range-request planning).
    pub offset: u64,
    /// Length of the compressed data in bytes.
    pub length: u64,
}

/// Bit packing order of uncompressed payloads exposed to the GPU unpacker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackOrder {
    /// Bits packed MSB-first, bytes in order (big-endian packed).
    MsbBytes,
    /// Bits packed LSB-first, bytes in order (little-endian packed).
    LsbBytes,
    /// MSB-first within 16-bit little-endian words.
    Msb16,
    /// MSB-first within 32-bit little-endian words.
    Msb32,
}

/// A uniformly bit-packed payload that can be unpacked without entropy decoding
/// (directly on the GPU, or with SIMD kernels).
#[derive(Debug, Clone, Copy)]
pub struct PackedPayload<'a> {
    /// Packed bytes, starting at the first row.
    pub data: &'a [u8],
    /// Width in samples.
    pub width: u32,
    /// Height in rows.
    pub height: u32,
    /// Bits per sample.
    pub bits: u32,
    /// Bit packing order.
    pub order: PackOrder,
    /// Bytes from the start of one row to the next.
    pub row_stride: usize,
}

/// A parsed RAW file ready for decoding.
pub trait RawDecoder: Send + Sync {
    /// Container format.
    fn format(&self) -> RawFormat;
    /// Normalised metadata.
    fn metadata(&self) -> &RawMetadata;
    /// Geometry and calibration of the sensor buffer.
    fn frame_info(&self) -> &FrameInfo;

    /// Decode the full sensor buffer.
    fn decode(&self, ctx: &DecodeContext) -> Result<RawImage>;

    /// How the payload can be randomly accessed.
    fn random_access(&self) -> RandomAccess {
        RandomAccess::Sequential
    }

    /// Independently decodable units (tiles/strips), if any.
    fn units(&self) -> Vec<DecodeUnit> {
        Vec::new()
    }

    /// Decode only `region` (buffer coordinates). Formats with random access override
    /// this to avoid decoding unrelated data; the default decodes everything and crops.
    fn decode_region(&self, ctx: &DecodeContext, region: Rect) -> Result<RawImage> {
        self.decode(ctx)?.crop(region)
    }

    /// The payload as a uniformly packed bit stream, when the format stores it that way.
    fn packed_payload(&self) -> Option<PackedPayload<'_>> {
        None
    }
}
