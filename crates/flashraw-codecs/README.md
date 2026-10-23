# flashraw-codecs

The RAW compression codecs of FlashRaw: lossless JPEG, packed 10/12/14-bit unpacking (SSSE3/SSE4.1, NEON and WASM SIMD128 kernels), deflate, and the vendor entropy coders.

This is an internal building block of [FlashRaw](https://github.com/ybouane/flashraw). It is published
because the `flashraw` crate depends on it, and it has **no semver guarantee of its own**: depend on
[`flashraw`](https://crates.io/crates/flashraw) instead, which re-exports what applications need.

* Documentation: <https://docs.rs/flashraw-codecs>
* Project overview and architecture: [README](https://github.com/ybouane/flashraw/blob/main/README.md), [architecture](https://github.com/ybouane/flashraw/blob/main/docs/architecture.md)
* Licence: MIT OR Apache-2.0 (`LICENSE-MIT`, `LICENSE-APACHE`)
