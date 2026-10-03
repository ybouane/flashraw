//! # flashraw-core
//!
//! Foundation crate of FlashRaw: structured errors, bounds-checked zero-copy byte
//! access, high-throughput bit pumps, Huffman decoding, TIFF/BMFF container
//! parsing, sensor-domain value types, and the [`RawDecoder`] contract that every
//! format implements.
//!
//! Most users want the `flashraw` crate instead.

#![deny(unsafe_op_in_unsafe_fn)]

pub mod bits;
pub mod bmff;
pub mod bytes;
pub mod decoder;
mod error;
pub mod exif;
pub mod huffman;
pub mod image;
pub mod metadata;
pub mod par;
pub mod profile;
pub mod simd;
pub mod tiff;
pub mod types;

pub use bytes::{ByteOrder, Cursor, ReadAt};
pub use decoder::{
    DecodeContext, DecodeUnit, FrameInfo, PackOrder, PackedPayload, RandomAccess, RawDecoder, SampleFormat,
};
pub use error::{Error, Result};
pub use image::{Limits, PixelData, RawImage, alloc_zeroed, max_nonneg_bits};
pub use metadata::{ExposureInfo, GpsInfo, LensInfo, PreviewEncoding, PreviewInfo, PreviewKind, RawMetadata};
pub use profile::{Profiler, Stage};
pub use types::*;
