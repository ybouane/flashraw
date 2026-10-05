//! Structured error type shared by every FlashRaw crate.

use std::fmt;

/// Result alias used throughout FlashRaw.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// All errors produced while parsing or decoding a RAW file.
///
/// Errors carry enough context (format, offset, what was being parsed) to make a
/// failure on a corpus file actionable without a debugger.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The bytes do not look like any RAW format FlashRaw understands.
    #[error("unsupported or unrecognised file format{}", fmt_ctx(.0))]
    UnsupportedFormat(String),

    /// The container was understood but the payload uses an unsupported compression scheme.
    #[error("unsupported compression {compression} in {format}")]
    UnsupportedCompression {
        /// Container format name (e.g. `"NEF"`).
        format: &'static str,
        /// Description of the compression scheme.
        compression: String,
    },

    /// The file is structurally invalid (truncated, bad offsets, corrupt entropy data...).
    #[error("corrupt file: {what}{}", .offset.map(|o| format!(" (at offset {o:#x})")).unwrap_or_default())]
    CorruptFile {
        /// What was being parsed or what was wrong.
        what: String,
        /// Byte offset in the input where the problem was found, when known.
        offset: Option<u64>,
    },

    /// Metadata required for decoding is missing or nonsensical.
    #[error("invalid metadata: {0}")]
    InvalidMetadata(String),

    /// The file format is supported but this camera or mode is not (yet).
    #[error("camera unsupported: {make} {model}{}", fmt_ctx(.reason))]
    CameraUnsupported {
        /// Camera make.
        make: String,
        /// Camera model.
        model: String,
        /// Why the camera or mode is unsupported (may be empty).
        reason: String,
    },

    /// A GPU backend was requested but is not available.
    #[error("GPU unavailable: {0}")]
    GpuUnavailable(String),

    /// An allocation would exceed configured limits or failed.
    #[error("out of memory: {0}")]
    OutOfMemory(String),

    /// Image dimensions or other values exceed configured safety limits.
    #[error("limit exceeded: {0}")]
    LimitExceeded(String),

    /// The requested operation is not possible for this file (e.g. no preview, no tiles).
    #[error("not available: {0}")]
    NotAvailable(String),

    /// Invalid caller-provided arguments (bad region, incompatible options...).
    #[error("invalid argument: {0}")]
    InvalidArgument(String),

    /// I/O failure while reading input.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

fn fmt_ctx(s: &str) -> String {
    if s.is_empty() { String::new() } else { format!(": {s}") }
}

impl Error {
    /// Shorthand for a [`Error::CorruptFile`] without an offset.
    #[cold]
    pub fn corrupt(what: impl fmt::Display) -> Self {
        Error::CorruptFile { what: what.to_string(), offset: None }
    }

    /// Shorthand for a [`Error::CorruptFile`] with an offset.
    #[cold]
    pub fn corrupt_at(what: impl fmt::Display, offset: u64) -> Self {
        Error::CorruptFile { what: what.to_string(), offset: Some(offset) }
    }

    /// Shorthand for an [`Error::InvalidMetadata`].
    #[cold]
    pub fn invalid_metadata(what: impl fmt::Display) -> Self {
        Error::InvalidMetadata(what.to_string())
    }

    /// Shorthand for an [`Error::UnsupportedCompression`].
    #[cold]
    pub fn unsupported_compression(format: &'static str, compression: impl fmt::Display) -> Self {
        Error::UnsupportedCompression { format, compression: compression.to_string() }
    }

    /// Short machine-friendly classification, used by the CLI's JSON output and the JS bindings.
    pub fn kind(&self) -> &'static str {
        match self {
            Error::UnsupportedFormat(_) => "UnsupportedFormat",
            Error::UnsupportedCompression { .. } => "UnsupportedCompression",
            Error::CorruptFile { .. } => "CorruptFile",
            Error::InvalidMetadata(_) => "InvalidMetadata",
            Error::CameraUnsupported { .. } => "CameraUnsupported",
            Error::GpuUnavailable(_) => "GpuUnavailable",
            Error::OutOfMemory(_) => "OutOfMemory",
            Error::LimitExceeded(_) => "LimitExceeded",
            Error::NotAvailable(_) => "NotAvailable",
            Error::InvalidArgument(_) => "InvalidArgument",
            Error::Io(_) => "Io",
        }
    }
}

/// Return early with a [`Error::CorruptFile`].
#[macro_export]
macro_rules! bail_corrupt {
    ($($arg:tt)*) => { return Err($crate::Error::corrupt(format!($($arg)*))) };
}

/// Ensure a condition holds, otherwise return a [`Error::CorruptFile`].
#[macro_export]
macro_rules! ensure_corrupt {
    ($cond:expr, $($arg:tt)*) => {
        if !$cond { return Err($crate::Error::corrupt(format!($($arg)*))); }
    };
}
