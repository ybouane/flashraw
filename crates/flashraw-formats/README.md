# flashraw-formats

The RAW container parsers and the per-vendor format decoders of FlashRaw (DNG, CR2, CR3, NEF, ARW, RAF, RW2, ORF, PEF and others). Format status: [docs/formats.md](https://github.com/ybouane/flashraw/blob/main/docs/formats.md).

This is an internal building block of [FlashRaw](https://github.com/ybouane/flashraw). It is published
because the `flashraw` crate depends on it, and it has **no semver guarantee of its own**: depend on
[`flashraw`](https://crates.io/crates/flashraw) instead, which re-exports what applications need.

* Documentation: <https://docs.rs/flashraw-formats>
* Project overview and architecture: [README](https://github.com/ybouane/flashraw/blob/main/README.md), [architecture](https://github.com/ybouane/flashraw/blob/main/docs/architecture.md)
* Licence: MIT OR Apache-2.0 (`LICENSE-MIT`, `LICENSE-APACHE`)
