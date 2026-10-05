//! Normalised camera/capture metadata and embedded preview descriptors.

use crate::types::{Compression, Orientation, RawFormat};

/// Lens information.
#[derive(Debug, Clone, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct LensInfo {
    /// Lens manufacturer, when recorded.
    pub make: Option<String>,
    /// Lens model name, when recorded.
    pub model: Option<String>,
    /// Lens serial number, when recorded.
    pub serial: Option<String>,
    /// Min/max focal length in mm.
    pub focal_range: Option<(f32, f32)>,
    /// Max aperture (f-number) at min/max focal length.
    pub aperture_range: Option<(f32, f32)>,
}

/// Exposure / capture parameters.
#[derive(Debug, Clone, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct ExposureInfo {
    /// ISO speed.
    pub iso: Option<u32>,
    /// Exposure time in seconds.
    pub exposure_time: Option<f64>,
    /// Aperture as an f-number (e.g. `2.8`).
    pub f_number: Option<f32>,
    /// Focal length in mm.
    pub focal_length: Option<f32>,
    /// 35 mm-equivalent focal length in mm (EXIF FocalLengthIn35mmFilm).
    pub focal_length_35mm: Option<u32>,
    /// Exposure compensation in EV.
    pub exposure_bias: Option<f32>,
    /// EXIF `Flash` tag value (bit field; bit 0 set when the flash fired).
    pub flash: Option<u16>,
    /// EXIF `MeteringMode` tag value.
    pub metering_mode: Option<u16>,
    /// EXIF `ExposureProgram` tag value.
    pub exposure_program: Option<u16>,
}

/// GPS position.
#[derive(Debug, Clone, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct GpsInfo {
    /// Decimal degrees, negative for south.
    pub latitude: Option<f64>,
    /// Decimal degrees, negative for west.
    pub longitude: Option<f64>,
    /// Metres; negative below sea level.
    pub altitude: Option<f64>,
    /// GPS date and time in UTC, `YYYY-MM-DDTHH:MM:SSZ`.
    pub timestamp: Option<String>,
}

/// Encoding of an embedded preview image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub enum PreviewEncoding {
    /// Baseline JPEG.
    Jpeg,
    /// Uncompressed 8-bit RGB, `width * height * 3` bytes.
    Rgb8,
    /// Uncompressed 8-bit YCbCr 4:2:2 (rare; some old Kodak/Sony files).
    Ycbcr422,
    /// JPEG XL (DNG 1.7 previews).
    JpegXl,
    /// HEIF/HEVC (e.g. CR3 HEIF previews, iPhone); exposed as bytes only.
    Heif,
}

/// An embedded preview / thumbnail stored in the RAW container.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct PreviewInfo {
    /// Byte offset of the preview data in the file.
    pub offset: u64,
    /// Length of the preview data in bytes.
    pub length: u64,
    /// Width in pixels as stored (before orientation); 0 if unknown.
    pub width: u32,
    /// Height in pixels as stored (before orientation); 0 if unknown.
    pub height: u32,
    /// How the preview bytes are encoded.
    pub encoding: PreviewEncoding,
    /// Orientation that applies to this preview (usually the file orientation).
    pub orientation: Orientation,
    /// What the container labels this image as.
    pub kind: PreviewKind,
}

impl PreviewInfo {
    /// A preview of `length` bytes at `offset` in the file.
    pub fn new(
        offset: u64,
        length: u64,
        width: u32,
        height: u32,
        encoding: PreviewEncoding,
        orientation: Orientation,
        kind: PreviewKind,
    ) -> Self {
        PreviewInfo { offset, length, width, height, encoding, orientation, kind }
    }
}

/// What the container labels a preview as.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub enum PreviewKind {
    /// Small thumbnail.
    Thumbnail,
    /// Reduced-size preview.
    Preview,
    /// Full-resolution rendering of the image.
    FullSize,
}

/// Normalised metadata for a RAW file, available without decoding sensor data.
#[derive(Debug, Clone, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct RawMetadata {
    /// Make as written in the file.
    pub make: String,
    /// Model as written in the file.
    pub model: String,
    /// Normalised maker name ("Canon", "Nikon", "Sony", ...).
    pub canonical_make: String,
    /// Normalised model name (aliases resolved).
    pub canonical_model: String,
    /// DNG UniqueCameraModel or equivalent.
    pub unique_camera_model: Option<String>,
    /// Writing software (EXIF `Software` tag).
    pub software: Option<String>,
    /// Camera firmware version, from maker notes where available.
    pub firmware: Option<String>,
    /// Camera body serial number.
    pub serial: Option<String>,
    /// Artist / author.
    pub artist: Option<String>,
    /// Copyright notice.
    pub copyright: Option<String>,
    /// Lens information (fields are `None` when not recorded).
    pub lens: LensInfo,
    /// Exposure parameters (fields are `None` when not recorded).
    pub exposure: ExposureInfo,
    /// Capture timestamp, `YYYY-MM-DDTHH:MM:SS[.sss][±HH:MM]` when available.
    pub capture_time: Option<String>,
    /// GPS position; `None` unless a latitude or longitude is recorded.
    pub gps: Option<GpsInfo>,
    /// Image orientation.
    pub orientation: Orientation,
    /// Container format, when identified.
    pub format: Option<RawFormat>,
    /// Payload compression of the main RAW image, when identified.
    pub compression: Option<Compression>,
    /// Bits per sample of the encoded data.
    pub bits_per_sample: u8,
    /// Full sensor buffer width (including masked borders).
    pub raw_width: u32,
    /// Full sensor buffer height (including masked borders).
    pub raw_height: u32,
    /// Width after the default crop.
    pub width: u32,
    /// Height after the default crop.
    pub height: u32,
    /// DNG version bytes (e.g. `[1, 4, 0, 0]`); `None` for non-DNG files.
    pub dng_version: Option<[u8; 4]>,
    /// Free-form, vendor-specific values worth exposing (mode names, quality, crop mode...).
    pub extra: Vec<(String, String)>,
    /// Embedded previews and thumbnails.
    pub previews: Vec<PreviewInfo>,
}

impl RawMetadata {
    /// Metadata with the given make and model as written in the file; everything else
    /// empty.
    pub fn new(make: impl Into<String>, model: impl Into<String>) -> Self {
        RawMetadata { make: make.into(), model: model.into(), ..Default::default() }
    }

    /// Set an entry of [`RawMetadata::extra`], replacing any existing value for `key`.
    pub fn set_extra(&mut self, key: &str, value: impl ToString) {
        let value = value.to_string();
        if let Some(e) = self.extra.iter_mut().find(|(k, _)| k == key) {
            e.1 = value;
        } else {
            self.extra.push((key.to_string(), value));
        }
    }
    /// Look up an entry of [`RawMetadata::extra`] by key.
    pub fn extra(&self, key: &str) -> Option<&str> {
        self.extra.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }
    /// Human-friendly "Make Model" string using canonical names when known.
    pub fn camera(&self) -> String {
        let make = if self.canonical_make.is_empty() { &self.make } else { &self.canonical_make };
        let model = if self.canonical_model.is_empty() { &self.model } else { &self.canonical_model };
        format!("{make} {model}").trim().to_string()
    }
    /// Largest preview.
    pub fn largest_preview(&self) -> Option<&PreviewInfo> {
        self.previews.iter().max_by_key(|p| (p.width as u64 * p.height as u64, p.length))
    }
    /// Smallest preview (thumbnail).
    pub fn smallest_preview(&self) -> Option<&PreviewInfo> {
        self.previews.iter().min_by_key(|p| (p.width as u64 * p.height as u64, p.length))
    }
}
