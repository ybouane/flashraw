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
pub mod ljpeg;
pub mod misc_packed;
pub mod packed;
pub mod vc5;
pub mod x3f;

pub use packed::{PackedLayout, unpack, unpack_row, unpack_row_scalar};
