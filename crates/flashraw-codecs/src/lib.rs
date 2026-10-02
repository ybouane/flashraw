//! # flashraw-codecs
//!
//! Reusable RAW compression codecs. Each codec decodes into caller-provided
//! buffers (or row sinks) and never allocates full-image intermediates.
//!
//! Codecs are independent of container formats so they can be fuzzed, benchmarked
//! and tested in isolation (see `fuzz/` and `benches/`).

pub mod canon_cr2;
pub mod canon_crw;
pub mod canon_crx;
pub mod canon_sraw;
pub mod dct12;
pub mod deflate;
pub mod dng_opcodes;
pub mod jpeg;
#[cfg(feature = "jpegxl")]
pub mod jxl;
pub mod ljpeg;
pub mod nikon_huffman;
pub mod nikon_packed;
pub mod packed;
pub mod pentax_huffman;
pub mod samsung;

pub use packed::{PackedLayout, unpack, unpack_row, unpack_row_scalar};
