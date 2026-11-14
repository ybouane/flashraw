//! Integrity of the sensor payload: structured truncation reports, decode warnings,
//! and the range checks every format runs before it reads sensor bytes.
//!
//! A strip, tile or other sensor-data range that extends past the end of the file is
//! corruption, and never silent. Under the recover policy (the default,
//! [`DecodeContext::recover`]) the decoder uses every complete sample it has, fills the
//! rest with zeros and records a [`DecodeWarning`]. Under the strict policy it fails the
//! decode with an [`Error::CorruptFile`] whose [`CorruptReason::Truncated`] carries the
//! expected range, the bytes actually present and the file length.
//!
//! Tolerances (data that is short but still complete):
//! - Only the bytes the geometry needs are required: the last row needs its samples,
//!   not its padding to the full stride, and a declared length beyond that need (end
//!   padding) may run past the end of the file.
//! - Formats addressed purely by geometry may read past a declared byte count that is
//!   too small when the file holds the bytes (some writers understate it).

use crate::{DecodeContext, Error, Result};
use std::fmt;

/// Which part of the sensor payload a report refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub enum DataUnit {
    /// The payload as a whole (formats that store it as one range).
    Payload,
    /// A TIFF strip (or an equivalent vendor strip), by index.
    Strip(u32),
    /// A tile, by index (row-major).
    Tile(u32),
    /// A separately stored plane, field or frame, by index.
    Plane(u32),
}

impl DataUnit {
    /// Index of the strip, tile or plane (`None` for [`DataUnit::Payload`]).
    pub fn index(&self) -> Option<u32> {
        match *self {
            DataUnit::Payload => None,
            DataUnit::Strip(i) | DataUnit::Tile(i) | DataUnit::Plane(i) => Some(i),
        }
    }

    /// `"payload"`, `"strip"`, `"tile"` or `"plane"`.
    pub fn kind(&self) -> &'static str {
        match self {
            DataUnit::Payload => "payload",
            DataUnit::Strip(_) => "strip",
            DataUnit::Tile(_) => "tile",
            DataUnit::Plane(_) => "plane",
        }
    }
}

impl fmt::Display for DataUnit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.index() {
            Some(i) => write!(f, "{} {i}", self.kind()),
            None => f.write_str("image data"),
        }
    }
}

/// A sensor-data range that the file does not fully contain.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct Truncation {
    /// The strip, tile or plane affected.
    pub unit: DataUnit,
    /// File offset where the unit starts.
    pub offset: u64,
    /// Bytes the unit needs from `offset` (its geometry, or its declared length).
    pub expected: u64,
    /// Bytes present from `offset` to the end of the file (0 when `offset` is past it).
    pub available: u64,
    /// Length of the input.
    pub file_len: u64,
}

impl Truncation {
    /// A report for `expected` bytes at `offset` of a `file_len`-byte input.
    pub fn new(unit: DataUnit, offset: u64, expected: u64, file_len: u64) -> Self {
        let available = file_len.saturating_sub(offset).min(expected);
        Truncation { unit, offset, expected, available, file_len }
    }

    /// Bytes missing from the unit.
    pub fn missing(&self) -> u64 {
        self.expected.saturating_sub(self.available)
    }

    /// True when the unit starts at or past the end of the file.
    pub fn starts_beyond_eof(&self) -> bool {
        self.offset >= self.file_len
    }

    /// True when `offset + expected` does not fit in the address space.
    pub fn range_overflows(&self) -> bool {
        self.offset.checked_add(self.expected).is_none_or(|e| usize::try_from(e).is_err())
    }
}

impl fmt::Display for Truncation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let unit = match self.unit {
            DataUnit::Payload => String::new(),
            u => format!("{u} "),
        };
        if self.range_overflows() {
            write!(
                f,
                "truncated image data: {unit}range of {} bytes at offset {} overflows (file is {} bytes)",
                self.expected, self.offset, self.file_len
            )
        } else if self.starts_beyond_eof() {
            write!(
                f,
                "truncated image data: {unit}expected {} bytes at offset {}, which is beyond the end of the file ({} bytes)",
                self.expected, self.offset, self.file_len
            )
        } else {
            write!(
                f,
                "truncated image data: {unit}expected {} bytes at offset {}, but only {} bytes remain before EOF",
                self.expected, self.offset, self.available
            )
        }
    }
}

/// Structured cause of an [`Error::CorruptFile`] (or of a recovered [`DecodeWarning`]).
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub enum CorruptReason {
    /// Sensor data extends past the end of the file.
    Truncated(Truncation),
    /// A compressed unit could not be decoded (reported as a warning when recovered).
    InvalidUnit {
        /// The strip or tile affected.
        unit: DataUnit,
        /// What the decoder reported.
        message: String,
    },
    /// The sensor data could not be decoded (e.g. an unsupported compression) and the
    /// frame holds the embedded full-size preview instead. Only reported as a warning, when
    /// the caller opted into the preview fallback.
    PreviewFallback {
        /// Why the sensor data was not decoded (the decoder's error).
        message: String,
    },
}

impl CorruptReason {
    /// The strip, tile or plane affected.
    pub fn unit(&self) -> DataUnit {
        match self {
            CorruptReason::Truncated(t) => t.unit,
            CorruptReason::InvalidUnit { unit, .. } => *unit,
            CorruptReason::PreviewFallback { .. } => DataUnit::Payload,
        }
    }

    /// Machine-friendly kind: `"truncated"`, `"invalid-unit"` or `"preview-fallback"`.
    pub fn kind(&self) -> &'static str {
        match self {
            CorruptReason::Truncated(_) => "truncated",
            CorruptReason::InvalidUnit { .. } => "invalid-unit",
            CorruptReason::PreviewFallback { .. } => "preview-fallback",
        }
    }

    /// The truncation details, for [`CorruptReason::Truncated`].
    pub fn truncation(&self) -> Option<&Truncation> {
        match self {
            CorruptReason::Truncated(t) => Some(t),
            _ => None,
        }
    }
}

impl fmt::Display for CorruptReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CorruptReason::Truncated(t) => t.fmt(f),
            CorruptReason::InvalidUnit { unit, message } => write!(f, "corrupt {unit}: {message}"),
            CorruptReason::PreviewFallback { message } => write!(f, "sensor data not decoded: {message}"),
        }
    }
}

/// Something a decode repaired or substituted instead of failing: under the recover policy
/// (truncated or corrupt units), or with the preview fallback (see
/// [`CorruptReason::PreviewFallback`]).
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct DecodeWarning {
    /// What was wrong.
    pub reason: CorruptReason,
    /// Output samples that hold no decoded data (set to zero).
    pub samples_filled: u64,
}

impl DecodeWarning {
    /// A warning for `reason` with `samples_filled` zero-filled samples.
    pub fn new(reason: CorruptReason, samples_filled: u64) -> Self {
        DecodeWarning { reason, samples_filled }
    }

    /// Machine-friendly kind: `"truncated"`, `"invalid-unit"` or `"preview-fallback"`.
    pub fn kind(&self) -> &'static str {
        self.reason.kind()
    }
}

impl fmt::Display for DecodeWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.reason {
            CorruptReason::Truncated(t) => write!(
                f,
                "recovered truncated {}: {} / {} bytes available; {} missing samples were zero-filled",
                t.unit, t.available, t.expected, self.samples_filled
            ),
            CorruptReason::InvalidUnit { unit, message } => {
                write!(f, "recovered corrupt {unit} ({message}); its {} samples were zero-filled", self.samples_filled)
            }
            CorruptReason::PreviewFallback { message } => {
                write!(f, "sensor data not decoded ({message}); the frame holds the embedded full-size preview")
            }
        }
    }
}

/// Geometry of a row-structured payload, used to work out how many bytes are needed
/// and how many samples a truncated range still holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RowGeometry {
    /// Rows in the unit.
    pub rows: usize,
    /// Bytes from one row start to the next.
    pub stride: usize,
    /// Bytes holding a row's samples (`<= stride`; the rest of the stride is padding).
    pub row_bytes: usize,
    /// Samples per row.
    pub samples: usize,
}

impl RowGeometry {
    /// Rows of `samples` values packed at `bits` per sample, `stride` bytes apart.
    pub fn packed(rows: usize, samples: usize, bits: u32, stride: usize) -> Self {
        let row_bytes = (samples.saturating_mul(bits as usize)).div_ceil(8);
        RowGeometry { rows, stride: stride.max(row_bytes), row_bytes, samples }
    }

    /// Tightly packed rows.
    pub fn tight(rows: usize, samples: usize, bits: u32) -> Self {
        let row_bytes = (samples.saturating_mul(bits as usize)).div_ceil(8);
        RowGeometry { rows, stride: row_bytes, row_bytes, samples }
    }

    /// Bytes needed for every sample: the last row needs no padding. Saturates.
    pub fn needed(&self) -> usize {
        if self.rows == 0 {
            return 0;
        }
        (self.rows - 1).saturating_mul(self.stride).saturating_add(self.row_bytes)
    }

    /// Total samples.
    pub fn total(&self) -> u64 {
        self.rows as u64 * self.samples as u64
    }

    /// Rows whose samples lie entirely within the first `avail` bytes.
    pub fn complete_rows(&self, avail: usize) -> usize {
        if avail < self.row_bytes || self.stride == 0 {
            return 0;
        }
        ((avail - self.row_bytes) / self.stride + 1).min(self.rows)
    }

    /// Samples entirely within the first `avail` bytes (complete rows plus the
    /// complete samples of a partial row).
    pub fn complete_samples(&self, avail: usize) -> u64 {
        let rows = self.complete_rows(avail);
        if rows >= self.rows {
            return self.total();
        }
        let rest = avail.saturating_sub(rows * self.stride).min(self.row_bytes);
        let partial = if self.row_bytes == 0 { 0 } else { rest as u64 * self.samples as u64 / self.row_bytes as u64 };
        rows as u64 * self.samples as u64 + partial
    }

    /// Samples not covered by the first `avail` bytes.
    pub fn missing_samples(&self, avail: usize) -> u64 {
        self.total() - self.complete_samples(avail)
    }
}

/// Check that `[offset, offset + needed)` lies within an input of `file_len` bytes.
/// Nothing is missing from an empty range, wherever it starts.
pub fn check_range(file_len: usize, unit: DataUnit, offset: usize, needed: usize) -> Result<(), Truncation> {
    if needed == 0 {
        return Ok(());
    }
    match offset.checked_add(needed) {
        Some(end) if end <= file_len => Ok(()),
        _ => Err(Truncation::new(unit, offset as u64, needed as u64, file_len as u64)),
    }
}

/// Sensor bytes of one unit, as resolved by [`DecodeContext::sensor_bytes`].
#[derive(Debug, Clone)]
pub struct SensorBytes<'a> {
    /// The unit's bytes: all `needed` bytes, or (recover policy only) what the file has.
    pub bytes: &'a [u8],
    /// Set when the unit is truncated (recover policy only): report it with
    /// [`DecodeContext::recovered`] once the samples it lacks are known.
    pub truncation: Option<Truncation>,
}

impl<'a> SensorBytes<'a> {
    /// True when the unit is complete.
    pub fn is_complete(&self) -> bool {
        self.truncation.is_none()
    }
}

impl DecodeContext<'_> {
    /// The bytes `[offset, offset + needed)` of sensor-data `unit` in `data` (the whole
    /// input). Under the strict policy a range not entirely in the file is an
    /// [`Error::CorruptFile`] carrying a [`Truncation`]; under the recover policy the
    /// available prefix (possibly empty) is returned with the truncation, which the caller
    /// reports through [`DecodeContext::recovered`] after zero-filling what is missing.
    pub fn sensor_bytes<'a>(
        &self,
        data: &'a [u8],
        unit: DataUnit,
        offset: usize,
        needed: usize,
    ) -> Result<SensorBytes<'a>> {
        match check_range(data.len(), unit, offset, needed) {
            Ok(()) => Ok(SensorBytes { bytes: data.get(offset..offset + needed).unwrap_or(&[]), truncation: None }),
            Err(t) if self.recover => {
                let start = offset.min(data.len());
                let end = start.saturating_add(needed).min(data.len());
                Ok(SensorBytes { bytes: &data[start..end], truncation: Some(t) })
            }
            Err(t) => Err(Error::truncated(t)),
        }
    }

    /// Like [`DecodeContext::sensor_bytes`] for a row-structured unit: `needed` comes
    /// from `geometry`, and a recovered truncation is reported immediately with the
    /// samples the available bytes do not cover (callers must leave those zero).
    pub fn sensor_rows<'a>(
        &self,
        data: &'a [u8],
        unit: DataUnit,
        offset: usize,
        geometry: RowGeometry,
    ) -> Result<&'a [u8]> {
        let s = self.sensor_bytes(data, unit, offset, geometry.needed())?;
        if let Some(t) = s.truncation {
            let filled = geometry.missing_samples(s.bytes.len());
            self.recovered(CorruptReason::Truncated(t), filled);
        }
        Ok(s.bytes)
    }

    /// Fail with `t` (strict), or record it as recovered with `samples_filled` zero
    /// samples (recover). For checks made outside [`DecodeContext::sensor_bytes`].
    pub fn truncated(&self, t: Truncation, samples_filled: u64) -> Result<()> {
        if self.recover {
            self.recovered(CorruptReason::Truncated(t), samples_filled);
            Ok(())
        } else {
            Err(Error::truncated(t))
        }
    }

    /// Record a problem the decode repaired (recover policy).
    pub fn recovered(&self, reason: CorruptReason, samples_filled: u64) {
        self.warnings.lock().unwrap_or_else(|p| p.into_inner()).push(DecodeWarning::new(reason, samples_filled));
    }

    /// Warnings recorded so far, sorted by unit (decoding may run units in parallel).
    pub fn take_warnings(&self) -> Vec<DecodeWarning> {
        let mut w = std::mem::take(&mut *self.warnings.lock().unwrap_or_else(|p| p.into_inner()));
        w.sort_by_key(|w| {
            let u = w.reason.unit();
            (u.kind(), u.index())
        });
        w
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Profiler;

    #[test]
    fn messages() {
        let t = Truncation::new(DataUnit::Strip(17), 24_576_000, 131_072, 24_576_000 + 48_291);
        assert_eq!(t.available, 48_291);
        assert_eq!(t.missing(), 131_072 - 48_291);
        assert_eq!(
            t.to_string(),
            "truncated image data: strip 17 expected 131072 bytes at offset 24576000, but only 48291 bytes remain before EOF"
        );
        let e = Error::truncated(t.clone());
        assert_eq!(e.kind(), "CorruptFile");
        assert_eq!(e.truncation(), Some(&t));
        assert!(e.to_string().starts_with("corrupt file: truncated image data: strip 17"));
        let w = DecodeWarning::new(CorruptReason::Truncated(t), 10);
        assert_eq!(
            w.to_string(),
            "recovered truncated strip 17: 48291 / 131072 bytes available; 10 missing samples were zero-filled"
        );
        let beyond = Truncation::new(DataUnit::Tile(2), 1000, 10, 500);
        assert!(beyond.starts_beyond_eof());
        assert_eq!(beyond.available, 0);
        assert!(beyond.to_string().contains("beyond the end of the file"));
        assert!(Truncation::new(DataUnit::Payload, u64::MAX - 1, 10, 5).range_overflows());
    }

    #[test]
    fn geometry() {
        // 3 rows of 4 12-bit samples (6 bytes) at a stride of 8.
        let g = RowGeometry::packed(3, 4, 12, 8);
        assert_eq!(g.needed(), 22);
        assert_eq!(g.complete_rows(22), 3);
        assert_eq!(g.complete_rows(21), 2);
        assert_eq!(g.complete_samples(21), 8 + 3);
        assert_eq!(g.missing_samples(0), 12);
        assert_eq!(g.missing_samples(100), 0);
    }

    #[test]
    fn policies() {
        let p = Profiler::new();
        let data = [0u8; 10];
        let mut ctx = DecodeContext::new(&p);
        assert!(ctx.recover, "recover is the default");
        ctx.recover = false;
        assert_eq!(ctx.sensor_bytes(&data, DataUnit::Payload, 2, 8).unwrap().bytes.len(), 8);
        let e = ctx.sensor_bytes(&data, DataUnit::Strip(1), 4, 8).unwrap_err();
        assert_eq!(e.truncation().unwrap().available, 6);
        assert!(ctx.sensor_bytes(&data, DataUnit::Strip(1), usize::MAX, 8).is_err());
        assert!(ctx.sensor_bytes(&data, DataUnit::Strip(1), 100, 0).unwrap().bytes.is_empty());
        ctx.recover = true;
        let s = ctx.sensor_bytes(&data, DataUnit::Strip(1), 4, 8).unwrap();
        assert_eq!(s.bytes.len(), 6);
        assert!(!s.is_complete());
        let s = ctx.sensor_bytes(&data, DataUnit::Strip(2), usize::MAX, 8).unwrap();
        assert!(s.bytes.is_empty());
        assert_eq!(ctx.sensor_rows(&data, DataUnit::Tile(0), 4, RowGeometry::tight(2, 4, 8)).unwrap().len(), 6);
        let w = ctx.take_warnings();
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].samples_filled, 2);
    }
}
