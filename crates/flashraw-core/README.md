# flashraw-core

Core types (`RawMetadata`, `Limits`, images, errors), bounds-checked byte readers, bit pumps, TIFF/BMFF container parsing and the parallel helpers used by every FlashRaw decoder.

This is an internal building block of [FlashRaw](https://github.com/ybouane/flashraw). It is published
because the `flashraw` crate depends on it, and it has **no semver guarantee of its own**: depend on
[`flashraw`](https://crates.io/crates/flashraw) instead, which re-exports what applications need.

* Documentation: <https://docs.rs/flashraw-core>
* Project overview and architecture: [README](https://github.com/ybouane/flashraw/blob/main/README.md), [architecture](https://github.com/ybouane/flashraw/blob/main/docs/architecture.md)
* Licence: MIT OR Apache-2.0 (`LICENSE-MIT`, `LICENSE-APACHE`)
