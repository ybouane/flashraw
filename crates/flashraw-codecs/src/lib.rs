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
pub mod fuji_compressed;
pub mod hasselblad;
pub mod jpeg;
#[cfg(feature = "jpegxl")]
pub mod jxl;
pub mod kodak;
pub mod leaf;
pub mod ljpeg;
mod ljpeg_par;
pub mod misc_packed;
pub mod nikon_he;
pub mod nikon_huffman;
pub mod nikon_packed;
pub mod olympus;
pub mod packed;
pub mod panasonic;
mod par_entropy;
pub mod pentax_huffman;
pub mod phaseone;
pub mod samsung;
pub mod sony_arw1;
pub mod sony_arw2;
pub mod sony_arw6;
pub mod sony_arw6_curve;
pub mod sony_ycc;
pub mod vc5;
pub mod x3f;

pub use packed::{PackedLayout, unpack, unpack_row, unpack_row_scalar};
