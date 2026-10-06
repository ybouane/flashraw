//! C-visible enums and plain structs, plus the size-versioned struct protocol.
//!
//! Enums are `#[repr(i32)]` so the header can declare them as `int32_t` typedefs. Values
//! arriving *from* C are always read as `int32_t` and validated (an out-of-range enum value
//! is an `INVALID_ARGUMENT` error, never undefined behaviour).

use crate::error::{FfiError, FfiResult};
use flashraw::{
    Backend, CfaColor, ColorSource, CorruptionPolicy, Demosaic, OutputFormat, PreviewEncoding, PreviewKind, Rect,
    SensorLayout, Stage,
};

/// Execution backend (`flashraw_decode_options.backend`).
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum flashraw_backend {
    /// Fastest available backend (CPU SIMD + threads unless the GPU backend is compiled in
    /// and predicted to win). The default.
    FLASHRAW_BACKEND_AUTO = 0,
    /// Portable scalar CPU code.
    FLASHRAW_BACKEND_CPU = 1,
    /// CPU with explicit SIMD kernels.
    FLASHRAW_BACKEND_CPU_SIMD = 2,
    /// GPU compute (needs a build with the `gpu` feature and an adapter).
    FLASHRAW_BACKEND_GPU = 3,
}

/// What a decode produces (`flashraw_decode_options.output`).
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum flashraw_output_format {
    /// Sensor samples as stored, `uint16_t` (floating-point data is scaled to 16 bits). The
    /// default.
    FLASHRAW_OUTPUT_RAW_U16 = 0,
    /// Sensor samples as `float`, not normalised.
    FLASHRAW_OUTPUT_RAW_F32 = 1,
    /// Black-subtracted, white-normalised sensor samples in [0, 1] (`float`, CFA layout kept).
    FLASHRAW_OUTPUT_NORMALIZED_F32 = 2,
    /// Demosaiced, white-balanced linear RGB with sRGB primaries, 3 `float`s per pixel. Sensor
    /// orientation (`flashraw_frame_orientation()` says what to apply).
    FLASHRAW_OUTPUT_LINEAR_RGB_F32 = 3,
    /// Display-ready 8-bit sRGB RGBA, orientation applied.
    FLASHRAW_OUTPUT_RGBA8 = 4,
    /// Display-ready 16-bit sRGB RGB (3 `uint16_t` per pixel, no alpha), orientation applied.
    FLASHRAW_OUTPUT_RGB16 = 5,
    /// `FLASHRAW_OUTPUT_LINEAR_RGB_F32` as 16-bit integers: [0, 1] clipped and scaled to
    /// [0, 65535], 3 `uint16_t` per pixel. Sensor orientation.
    FLASHRAW_OUTPUT_LINEAR_RGB16 = 6,
    /// `FLASHRAW_OUTPUT_LINEAR_RGB_F32` as IEEE half floats (`FLASHRAW_SAMPLE_F16`), half the
    /// memory; values beyond +-65504 saturate. Sensor orientation.
    FLASHRAW_OUTPUT_LINEAR_RGB_F16 = 7,
}

/// Demosaic algorithm for the RGB outputs (`flashraw_decode_options.demosaic`).
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum flashraw_demosaic {
    /// Bilinear interpolation. The default.
    FLASHRAW_DEMOSAIC_BILINEAR = 0,
    /// 2x2 superpixel binning: half resolution, fastest.
    FLASHRAW_DEMOSAIC_HALF_SIZE = 1,
}

/// Behaviour on truncated or corrupt sensor data (`flashraw_decode_options.corruption_policy`).
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum flashraw_corruption_policy {
    /// Decode what is present, zero-fill the rest and report it as frame warnings
    /// (`flashraw_frame_is_incomplete()`). The default.
    FLASHRAW_CORRUPTION_RECOVER = 0,
    /// Fail with `FLASHRAW_ERROR_CORRUPT_FILE`.
    FLASHRAW_CORRUPTION_STRICT = 1,
}

/// Where a frame's pixels come from (`flashraw_frame_source()`).
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum flashraw_pixel_source {
    /// Decoded from the sensor data.
    FLASHRAW_SOURCE_SENSOR = 0,
    /// The embedded full-size preview (`flashraw_decode_options.preview_fallback`); the frame
    /// also carries a `preview-fallback` warning.
    FLASHRAW_SOURCE_PREVIEW = 1,
}

/// Element type of a frame's sample buffer.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum flashraw_sample_type {
    /// `uint8_t` (RGBA8 output).
    FLASHRAW_SAMPLE_U8 = 1,
    /// `uint16_t` (raw-u16, rgb16 and linear-rgb16 outputs).
    FLASHRAW_SAMPLE_U16 = 2,
    /// `float` (raw-f32, normalized and linear-rgb outputs).
    FLASHRAW_SAMPLE_F32 = 3,
    /// IEEE 754 binary16 (half float) bit patterns in `uint16_t` (linear-rgb-f16 output).
    FLASHRAW_SAMPLE_F16 = 4,
}

/// Colour of one CFA site (DNG colour indices, plus emerald).
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum flashraw_cfa_color {
    FLASHRAW_CFA_RED = 0,
    FLASHRAW_CFA_GREEN = 1,
    FLASHRAW_CFA_BLUE = 2,
    FLASHRAW_CFA_CYAN = 3,
    FLASHRAW_CFA_MAGENTA = 4,
    FLASHRAW_CFA_YELLOW = 5,
    FLASHRAW_CFA_WHITE = 6,
    FLASHRAW_CFA_EMERALD = 7,
}

/// How samples are arranged on the sensor.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum flashraw_sensor_layout {
    /// 2x2 Bayer CFA, one sample per pixel.
    FLASHRAW_LAYOUT_BAYER = 0,
    /// 6x6 X-Trans CFA, one sample per pixel.
    FLASHRAW_LAYOUT_XTRANS = 1,
    /// Quad Bayer (2x2 same-colour blocks in a 4x4 repeat).
    FLASHRAW_LAYOUT_QUAD_BAYER = 2,
    /// Any other repeating CFA (CYGM, RGBE, ...).
    FLASHRAW_LAYOUT_OTHER_CFA = 3,
    /// No CFA: one luminance sample per pixel.
    FLASHRAW_LAYOUT_MONOCHROME = 4,
    /// Three or more samples per pixel (linear DNG, sRAW, RGB outputs).
    FLASHRAW_LAYOUT_LINEAR = 5,
    /// Layered (Foveon) sensor, three samples per pixel.
    FLASHRAW_LAYOUT_FOVEON = 6,
    /// A layout added after this ABI version.
    FLASHRAW_LAYOUT_UNKNOWN = 255,
}

/// Encoding of an embedded preview.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum flashraw_preview_encoding {
    FLASHRAW_PREVIEW_JPEG = 0,
    /// Uncompressed 8-bit RGB, `width * height * 3` bytes.
    FLASHRAW_PREVIEW_RGB8 = 1,
    FLASHRAW_PREVIEW_YCBCR422 = 2,
    FLASHRAW_PREVIEW_JPEG_XL = 3,
    FLASHRAW_PREVIEW_HEIF = 4,
    FLASHRAW_PREVIEW_UNKNOWN = 255,
}

/// What the container labels a preview as.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum flashraw_preview_kind {
    FLASHRAW_PREVIEW_KIND_THUMBNAIL = 0,
    FLASHRAW_PREVIEW_KIND_PREVIEW = 1,
    FLASHRAW_PREVIEW_KIND_FULL_SIZE = 2,
    FLASHRAW_PREVIEW_KIND_UNKNOWN = 255,
}

/// Which colour matrix list to query.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum flashraw_matrix_kind {
    /// XYZ -> camera matrices (DNG ColorMatrix1/2/3 or database matrix).
    FLASHRAW_MATRIX_COLOR = 0,
    /// Camera -> XYZ (D50) forward matrices (DNG ForwardMatrix1/2/3).
    FLASHRAW_MATRIX_FORWARD = 1,
    /// Per-camera calibration (DNG CameraCalibration1/2/3).
    FLASHRAW_MATRIX_CALIBRATION = 2,
}

/// Where the colour calibration came from.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum flashraw_color_source {
    FLASHRAW_COLOR_SOURCE_NONE = 0,
    /// Embedded in the file.
    FLASHRAW_COLOR_SOURCE_FILE = 1,
    /// From the FlashRaw camera database.
    FLASHRAW_COLOR_SOURCE_CAMERA_DB = 2,
    FLASHRAW_COLOR_SOURCE_UNKNOWN = 255,
}

/// Pipeline stage of a timing entry.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum flashraw_stage {
    FLASHRAW_STAGE_CONTAINER_PARSE = 0,
    FLASHRAW_STAGE_METADATA = 1,
    FLASHRAW_STAGE_ENTROPY_DECODE = 2,
    FLASHRAW_STAGE_UNPACK = 3,
    FLASHRAW_STAGE_PREDICTOR = 4,
    FLASHRAW_STAGE_CALIBRATION = 5,
    FLASHRAW_STAGE_POSTPROCESS = 6,
    FLASHRAW_STAGE_GPU_UPLOAD = 7,
    FLASHRAW_STAGE_GPU_COMPUTE = 8,
    FLASHRAW_STAGE_GPU_DOWNLOAD = 9,
    FLASHRAW_STAGE_PREVIEW = 10,
    FLASHRAW_STAGE_OTHER = 255,
}

/// Metadata strings available through `flashraw_file_string()`.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum flashraw_string_key {
    /// Make as recorded in the file.
    FLASHRAW_STRING_MAKE = 0,
    /// Model as recorded in the file.
    FLASHRAW_STRING_MODEL = 1,
    /// Normalised make (e.g. "Nikon" for "NIKON CORPORATION").
    FLASHRAW_STRING_CANONICAL_MAKE = 2,
    /// Normalised model.
    FLASHRAW_STRING_CANONICAL_MODEL = 3,
    /// "<canonical make> <canonical model>".
    FLASHRAW_STRING_CAMERA = 4,
    /// Container format name ("DNG", "NEF", "CR3", ...).
    FLASHRAW_STRING_FORMAT = 5,
    /// Compression scheme name.
    FLASHRAW_STRING_COMPRESSION = 6,
    FLASHRAW_STRING_SOFTWARE = 7,
    FLASHRAW_STRING_FIRMWARE = 8,
    /// Camera body serial number.
    FLASHRAW_STRING_SERIAL = 9,
    FLASHRAW_STRING_ARTIST = 10,
    FLASHRAW_STRING_COPYRIGHT = 11,
    /// Capture time, `YYYY-MM-DDTHH:MM:SS[.sss][+HH:MM]`.
    FLASHRAW_STRING_CAPTURE_TIME = 12,
    FLASHRAW_STRING_LENS_MAKE = 13,
    FLASHRAW_STRING_LENS_MODEL = 14,
    FLASHRAW_STRING_LENS_SERIAL = 15,
    /// DNG UniqueCameraModel.
    FLASHRAW_STRING_UNIQUE_CAMERA_MODEL = 16,
}

/// Number of `flashraw_string_key` values.
pub(crate) const STRING_KEYS: usize = 17;

/// A rectangle in pixels.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct flashraw_rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl From<Rect> for flashraw_rect {
    fn from(r: Rect) -> Self {
        flashraw_rect { x: r.x, y: r.y, width: r.width, height: r.height }
    }
}

impl From<flashraw_rect> for Rect {
    fn from(r: flashraw_rect) -> Self {
        Rect::new(r.x, r.y, r.width, r.height)
    }
}

/// A repeating CFA pattern, anchored at sensor-buffer (0, 0).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct flashraw_cfa {
    /// Pattern width (2 for Bayer, 6 for X-Trans; at most 8).
    pub width: u32,
    /// Pattern height.
    pub height: u32,
    /// `width * height` `flashraw_cfa_color` values, row-major; the rest is zero.
    pub colors: [u8; 64],
}

/// Black level description (values via the `values` array of the getter).
///
/// The black for sample `c` of pixel `(x, y)` is
/// `values[((y % rows) * cols + (x % cols)) * samples + (c % samples)]`, plus per-row and
/// per-column deltas when `has_deltas` is set (available in the metadata JSON).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct flashraw_black_level {
    pub rows: u32,
    pub cols: u32,
    pub samples: u32,
    /// Number of pattern values (`rows * cols * samples`).
    pub count: u32,
    /// Mean of the pattern.
    pub mean: f32,
    /// Non-zero when per-row / per-column deltas exist (DNG BlackLevelDeltaV/H).
    pub has_deltas: u32,
}

/// One colour matrix.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct flashraw_color_matrix {
    /// EXIF LightSource / DNG CalibrationIlluminant code (21 = D65, 17 = A, ...).
    pub illuminant: u32,
    /// Rows (camera channels for colour matrices).
    pub rows: u32,
    /// Columns.
    pub cols: u32,
    /// `rows * cols` coefficients, row-major.
    pub values: [f32; 16],
}

/// An embedded preview (size-versioned output struct: set `size` before the call).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct flashraw_preview_info {
    /// `sizeof(flashraw_preview_info)`, set by the caller.
    pub size: u32,
    /// Width in pixels as stored (0 if unknown).
    pub width: u32,
    /// Height in pixels as stored (0 if unknown).
    pub height: u32,
    /// `flashraw_preview_encoding`.
    pub encoding: i32,
    /// `flashraw_preview_kind`.
    pub kind: i32,
    /// EXIF orientation (1-8) to apply to the preview's pixels.
    pub orientation: u32,
    /// Byte offset of the preview in the file.
    pub offset: u64,
    /// Length of the preview in bytes.
    pub length: u64,
}

/// Decode statistics (size-versioned output struct: set `size` before the call).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct flashraw_decode_stats {
    /// `sizeof(flashraw_decode_stats)`, set by the caller.
    pub size: u32,
    /// Worker threads available to the decode.
    pub threads: u32,
    /// Wall-clock milliseconds, including container parsing.
    pub total_ms: f64,
    /// Size of the input in bytes.
    pub input_bytes: u64,
    /// Size of the output buffer in bytes.
    pub output_bytes: u64,
    /// Milliseconds per `flashraw_stage` (index = stage value, 0..=10).
    pub stage_ms: [f64; 11],
}

/// Options for opening a file (size-versioned input struct; see
/// `flashraw_open_options_init()`). Pass NULL for the defaults.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct flashraw_open_options {
    /// `sizeof(flashraw_open_options)`.
    pub size: u32,
    /// Maximum width or height of a decoded image (0 = default, 65535).
    pub max_dimension: u32,
    /// Maximum number of samples in a decoded image (0 = default, 1.2 billion).
    pub max_samples: u64,
}

/// Options for `flashraw_decode()` (size-versioned input struct; see
/// `flashraw_decode_options_init()`). Pass NULL for the defaults.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct flashraw_decode_options {
    /// `sizeof(flashraw_decode_options)`, set by `flashraw_decode_options_init()`.
    pub size: u32,
    /// `flashraw_backend`. Default `FLASHRAW_BACKEND_AUTO`.
    pub backend: i32,
    /// `flashraw_output_format`. Default `FLASHRAW_OUTPUT_RAW_U16`.
    pub output: i32,
    /// `flashraw_demosaic`. Default `FLASHRAW_DEMOSAIC_BILINEAR`.
    pub demosaic: i32,
    /// `flashraw_corruption_policy`. Default `FLASHRAW_CORRUPTION_RECOVER`.
    pub corruption_policy: i32,
    /// Worker threads; 0 = all cores (default), 1 = single-threaded.
    pub threads: u32,
    /// Non-zero: crop RGB outputs to the recommended default crop (default 1). Raw outputs
    /// are never cropped implicitly.
    pub crop: i32,
    /// Non-zero: apply DNG opcode lists (default 1).
    pub apply_opcodes: i32,
    /// Non-zero: decode only `region`.
    pub use_region: i32,
    /// Region to decode, in sensor-buffer coordinates (used when `use_region` is non-zero).
    pub region: flashraw_rect,
    /// Maximum width or height (0 = the file's open limit).
    pub max_dimension: u32,
    /// Maximum number of samples (0 = the file's open limit).
    pub max_samples: u64,
    /// RGB outputs: exposure in EV, applied to linear values after the colour conversion and
    /// before the black/white points and the output encoding (range [-32, 32]; default 0).
    pub exposure: f32,
    /// RGB outputs: linear value (after exposure) mapped to black (default 0).
    pub black_point: f32,
    /// RGB outputs: linear value (after exposure) mapped to white; must be above
    /// `black_point` (default 1). Integer outputs clip outside [black, white].
    pub white_point: f32,
    /// Non-zero: when the sensor data cannot be decoded (`FLASHRAW_ERROR_UNSUPPORTED_COMPRESSION`,
    /// e.g. Nikon High Efficiency NEF), RGB outputs return the embedded full-size preview
    /// instead (`flashraw_frame_source()` is then `FLASHRAW_SOURCE_PREVIEW`). Default 0: the
    /// error is returned.
    pub preview_fallback: i32,
}

/// Size of the first published layout of each versioned struct. Callers built against an
/// older header pass these sizes; the fields they do not know take their defaults.
pub(crate) trait Versioned: Copy {
    const MIN_SIZE: usize;
}
impl Versioned for flashraw_open_options {
    const MIN_SIZE: usize = 16;
}
impl Versioned for flashraw_decode_options {
    const MIN_SIZE: usize = 64;
}
impl Versioned for flashraw_preview_info {
    const MIN_SIZE: usize = 40;
}
impl Versioned for flashraw_decode_stats {
    const MIN_SIZE: usize = 120;
}

/// Largest `size` accepted (guards against garbage in the size field).
const MAX_STRUCT_SIZE: usize = 4096;

/// Read a size-versioned input struct: the first `min(size, sizeof(T))` bytes come from the
/// caller, the rest keep the values of `defaults`. A caller struct larger than ours (built
/// against a newer header) is accepted only if the fields we do not know are all zero.
///
/// # Safety
/// `p` must be NULL or point to a readable struct whose first field is a `uint32_t` size
/// that is no larger than the allocation.
pub(crate) unsafe fn read_versioned<T: Versioned>(p: *const T, defaults: T, what: &str) -> FfiResult<T> {
    if p.is_null() {
        return Ok(defaults);
    }
    // SAFETY: `p` is non-null and, per the contract, starts with a readable u32.
    let size = unsafe { (p as *const u32).read_unaligned() } as usize;
    let ours = std::mem::size_of::<T>();
    if !(T::MIN_SIZE..=MAX_STRUCT_SIZE).contains(&size) {
        return Err(FfiError::invalid(format!(
            "`{what}.size` is {size}; set it with the matching `_init()` function (expected {ours})"
        )));
    }
    // SAFETY: the caller guarantees `size` readable bytes at `p`.
    let bytes = unsafe { std::slice::from_raw_parts(p as *const u8, size) };
    if size > ours && bytes[ours..].iter().any(|&b| b != 0) {
        return Err(FfiError::invalid(format!(
            "`{what}` sets fields unknown to this library version ({size} bytes, this version knows {ours})"
        )));
    }
    let mut out = defaults;
    let n = size.min(ours);
    // SAFETY: `out` is a valid `T` of `ours >= n` bytes; every versioned struct consists of
    // integer and float fields only (no padding, no bools, no pointers), so any byte pattern
    // is a valid value.
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), &mut out as *mut T as *mut u8, n) };
    Ok(out)
}

/// Write a size-versioned output struct: copies `min(size, sizeof(T))` bytes of `value`
/// after the size field (which is left as the caller set it).
///
/// # Safety
/// `p` must be NULL or point to a writable struct whose first field is a `uint32_t` size
/// that is no larger than the allocation.
pub(crate) unsafe fn write_versioned<T: Versioned>(p: *mut T, value: &T, what: &str) -> FfiResult<()> {
    if p.is_null() {
        return Err(FfiError::null(what));
    }
    // SAFETY: `p` is non-null and starts with a readable u32 per the contract.
    let size = unsafe { (p as *const u32).read_unaligned() } as usize;
    if !(T::MIN_SIZE..=MAX_STRUCT_SIZE).contains(&size) {
        return Err(FfiError::invalid(format!(
            "`{what}->size` is {size}; set it to `sizeof` before the call (expected {})",
            std::mem::size_of::<T>()
        )));
    }
    let n = size.min(std::mem::size_of::<T>());
    // SAFETY: `p` has at least `size >= n` writable bytes; `value` has `size_of::<T>() >= n`
    // readable bytes; both start with the 4-byte size field, which is skipped.
    unsafe {
        std::ptr::copy_nonoverlapping((value as *const T as *const u8).add(4), (p as *mut u8).add(4), n - 4);
    }
    Ok(())
}

pub(crate) fn backend_from(v: i32) -> FfiResult<Backend> {
    Ok(match v {
        0 => Backend::Auto,
        1 => Backend::Cpu,
        2 => Backend::CpuSimd,
        3 => Backend::Gpu,
        _ => return Err(FfiError::invalid(format!("unknown backend {v}"))),
    })
}

pub(crate) fn output_from(v: i32) -> FfiResult<OutputFormat> {
    Ok(match v {
        0 => OutputFormat::RawU16,
        1 => OutputFormat::RawF32,
        2 => OutputFormat::NormalizedF32,
        3 => OutputFormat::LinearRgbF32,
        4 => OutputFormat::Rgba8,
        5 => OutputFormat::Rgb16,
        6 => OutputFormat::LinearRgb16,
        7 => OutputFormat::LinearRgbF16,
        _ => return Err(FfiError::invalid(format!("unknown output format {v}"))),
    })
}

pub(crate) fn output_to(o: OutputFormat) -> i32 {
    match o.host_equivalent() {
        OutputFormat::RawU16 => 0,
        OutputFormat::RawF32 => 1,
        OutputFormat::NormalizedF32 => 2,
        OutputFormat::LinearRgbF32 => 3,
        OutputFormat::Rgba8 => 4,
        OutputFormat::Rgb16 => 5,
        OutputFormat::LinearRgb16 => 6,
        OutputFormat::LinearRgbF16 => 7,
        _ => -1,
    }
}

pub(crate) fn demosaic_from(v: i32) -> FfiResult<Demosaic> {
    Ok(match v {
        0 => Demosaic::Bilinear,
        1 => Demosaic::HalfSize,
        _ => return Err(FfiError::invalid(format!("unknown demosaic {v}"))),
    })
}

pub(crate) fn policy_from(v: i32) -> FfiResult<CorruptionPolicy> {
    Ok(match v {
        0 => CorruptionPolicy::Recover,
        1 => CorruptionPolicy::Strict,
        _ => return Err(FfiError::invalid(format!("unknown corruption policy {v}"))),
    })
}

pub(crate) fn cfa_color_to(c: CfaColor) -> u8 {
    c as u8
}

pub(crate) fn layout_to(l: SensorLayout) -> flashraw_sensor_layout {
    use flashraw_sensor_layout::*;
    match l {
        SensorLayout::Bayer => FLASHRAW_LAYOUT_BAYER,
        SensorLayout::XTrans => FLASHRAW_LAYOUT_XTRANS,
        SensorLayout::QuadBayer => FLASHRAW_LAYOUT_QUAD_BAYER,
        SensorLayout::OtherCfa => FLASHRAW_LAYOUT_OTHER_CFA,
        SensorLayout::Monochrome => FLASHRAW_LAYOUT_MONOCHROME,
        SensorLayout::Linear => FLASHRAW_LAYOUT_LINEAR,
        SensorLayout::Foveon => FLASHRAW_LAYOUT_FOVEON,
        _ => FLASHRAW_LAYOUT_UNKNOWN,
    }
}

pub(crate) fn encoding_to(e: PreviewEncoding) -> i32 {
    match e {
        PreviewEncoding::Jpeg => 0,
        PreviewEncoding::Rgb8 => 1,
        PreviewEncoding::Ycbcr422 => 2,
        PreviewEncoding::JpegXl => 3,
        PreviewEncoding::Heif => 4,
        _ => 255,
    }
}

pub(crate) fn kind_to(k: PreviewKind) -> i32 {
    match k {
        PreviewKind::Thumbnail => 0,
        PreviewKind::Preview => 1,
        PreviewKind::FullSize => 2,
        _ => 255,
    }
}

pub(crate) fn color_source_to(s: ColorSource) -> flashraw_color_source {
    use flashraw_color_source::*;
    match s {
        ColorSource::None => FLASHRAW_COLOR_SOURCE_NONE,
        ColorSource::File => FLASHRAW_COLOR_SOURCE_FILE,
        ColorSource::CameraDb => FLASHRAW_COLOR_SOURCE_CAMERA_DB,
        _ => FLASHRAW_COLOR_SOURCE_UNKNOWN,
    }
}

pub(crate) fn stage_to(s: Stage) -> Option<usize> {
    Some(match s {
        Stage::ContainerParse => 0,
        Stage::Metadata => 1,
        Stage::EntropyDecode => 2,
        Stage::Unpack => 3,
        Stage::Predictor => 4,
        Stage::Calibration => 5,
        Stage::Postprocess => 6,
        Stage::GpuUpload => 7,
        Stage::GpuCompute => 8,
        Stage::GpuDownload => 9,
        Stage::Preview => 10,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::size_of;

    #[test]
    fn versioned_sizes_match_first_layout() {
        // These are ABI: the v1 layouts. Growing a struct is allowed (append fields);
        // changing these numbers is not.
        assert_eq!(size_of::<flashraw_open_options>(), flashraw_open_options::MIN_SIZE);
        assert_eq!(flashraw_decode_options::MIN_SIZE, 64);
        // Grown since: exposure, black_point, white_point, preview_fallback.
        assert_eq!(size_of::<flashraw_decode_options>(), 80);
        assert_eq!(size_of::<flashraw_preview_info>(), flashraw_preview_info::MIN_SIZE);
        assert_eq!(size_of::<flashraw_decode_stats>(), flashraw_decode_stats::MIN_SIZE);
        assert_eq!(size_of::<flashraw_rect>(), 16);
        assert_eq!(size_of::<flashraw_cfa>(), 72);
        assert_eq!(size_of::<flashraw_black_level>(), 24);
        assert_eq!(size_of::<flashraw_color_matrix>(), 76);
    }
}
