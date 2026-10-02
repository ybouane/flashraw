//! # flashraw-codecs
//!
//! Reusable RAW compression codecs. Each codec decodes into caller-provided
//! buffers (or row sinks) and never allocates full-image intermediates.
//!
//! Codecs are independent of container formats so they can be fuzzed, benchmarked
//! and tested in isolation (see `fuzz/` and `benches/`).

pub mod deflate;
pub mod jpeg;
#[cfg(feature = "jpegxl")]
pub mod jxl;
pub mod leaf;
pub mod ljpeg;
pub mod packed;
pub mod phaseone;

pub use packed::{PackedLayout, unpack, unpack_row, unpack_row_scalar};
