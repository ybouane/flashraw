//! # LibRaw-compatible C API for FlashRaw
//!
//! An independent implementation of a pragmatic subset of the LibRaw C API (`libraw_*`
//! functions and the `libraw_data_t` data model) on top of FlashRaw, so programs written
//! against that API can switch by recompiling against `include/libraw/libraw.h` and linking
//! this library (`-lraw`). It contains no LibRaw code. The supported subset, every
//! deviation and the licensing notes are in `docs/libraw-compat.md`.
//!
//! Layout: `types` mirrors the C structures, `processor` opens and unpacks files into
//! them, `develop` implements `libraw_dcraw_process` and the output stage, `color` holds the
//! conversions between the two data models, and `api` the exported functions.

#![allow(non_camel_case_types, non_snake_case)]
#![deny(unsafe_op_in_unsafe_fn)]

pub mod api;
mod color;
mod develop;
mod processor;
pub mod types;

pub use api::*;
pub use types::*;
