//! Normalised camera/capture metadata and embedded preview descriptors.

use crate::types::{Compression, Orientation, RawFormat};

/// Lens information.
#[derive(Debug, Clone, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct LensInfo {
    pub make: Option<String>,
    pub model: Option<String>,
    pub serial: Option<String>,
    /// Min/max focal length in mm.
    pub focal_range: Option<(f32, f32)>,
    /// Max aperture (f-number) at min/max focal length.
    pub aperture_range: Option<(f32, f32)>,
}

/// Exposure / capture parameters.
#[derive(Debug, Clone, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ExposureInfo {
    pub iso: Option<u32>,
    /// Exposure time in seconds.
    pub exposure_time: Option<f64>,
    pub f_number: Option<f32>,
    /// Focal length in mm.
    pub focal_length: Option<f32>,
    pub focal_length_35mm: Option<u32>,
    pub exposure_bias: Option<f32>,
    pub flash: Option<u16>,
    pub metering_mode: Option<u16>,
    pub exposure_program: Option<u16>,
}

/// GPS position.
#[derive(Debug, Clone, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct GpsInfo {
    /// Decimal degrees, negative for south.
    pub latitude: Option<f64>,
    /// Decimal degrees, negative for west.
    pub longitude: Option<f64>,
    /// Metres; negative below sea level.
    pub altitude: Option<f64>,
    pub timestamp: Option<String>,
}

/// Encoding of an embedded preview image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum PreviewEncoding {
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
pub struct PreviewInfo {
    pub offset: u64,
    pub length: u64,
    pub width: u32,
    pub height: u32,
    pub encoding: PreviewEncoding,
    /// Orientation that applies to this preview (usually the file orientation).
    pub orientation: Orientation,
    /// What the container labels this image as.
    pub kind: PreviewKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum PreviewKind {
    Thumbnail,
    Preview,
    FullSize,
}

/// Normalised metadata for a RAW file, available without decoding sensor data.
#[derive(Debug, Clone, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
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
    pub software: Option<String>,
    pub firmware: Option<String>,
    pub serial: Option<String>,
    pub artist: Option<String>,
    pub copyright: Option<String>,
    pub lens: LensInfo,
    pub exposure: ExposureInfo,
    /// Capture timestamp, `YYYY-MM-DDTHH:MM:SS[.sss][±HH:MM]` when available.
    pub capture_time: Option<String>,
    pub gps: Option<GpsInfo>,
    pub orientation: Orientation,
    pub format: Option<RawFormat>,
    pub compression: Option<Compression>,
    /// Bits per sample of the encoded data.
    pub bits_per_sample: u8,
    /// Full sensor buffer dimensions.
    pub raw_width: u32,
    pub raw_height: u32,
    /// Dimensions after the default crop.
    pub width: u32,
    pub height: u32,
    pub dng_version: Option<[u8; 4]>,
    /// Free-form, vendor-specific values worth exposing (mode names, quality, crop mode...).
    pub extra: Vec<(String, String)>,
    pub previews: Vec<PreviewInfo>,
}

impl RawMetadata {
    pub fn set_extra(&mut self, key: &str, value: impl ToString) {
        let value = value.to_string();
        if let Some(e) = self.extra.iter_mut().find(|(k, _)| k == key) {
            e.1 = value;
        } else {
            self.extra.push((key.to_string(), value));
        }
    }
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
