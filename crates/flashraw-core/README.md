# flashraw-core

<!-- flashraw-intro -->
<p align="center"><a href="https://flashraw.ybouane.com"><img src="https://raw.githubusercontent.com/ybouane/flashraw/main/branding/readme-banner.png" alt="FlashRaw: the fast, safe camera RAW decoder" width="100%"></a></p>

**[FlashRaw](https://flashraw.ybouane.com)** is a fast, memory-safe camera RAW decoder written in Rust, with Rust, C, C++, Python and JavaScript (WebAssembly) bindings. It decodes about 40 RAW formats (CR2, CR3, NEF, ARW, RAF, DNG, RW2, ORF, PEF and more) with SIMD kernels, multithreading and an optional GPU backend, converts to display-ready RGB with ICC colour management, and runs in the browser.

**Official website and live demo: [flashraw.ybouane.com](https://flashraw.ybouane.com)** (it decodes your file locally in the browser) · [Source on GitHub](https://github.com/ybouane/flashraw) · Follow [@ybouane on X](https://x.com/ybouane)
<!-- /flashraw-intro -->

Core types (`RawMetadata`, `Limits`, images, errors), bounds-checked byte readers, bit pumps, TIFF/BMFF container parsing and the parallel helpers used by every FlashRaw decoder.

This is an internal building block of [FlashRaw](https://github.com/ybouane/flashraw). It is published
because the `flashraw` crate depends on it, and it has **no semver guarantee of its own**: depend on
[`flashraw`](https://crates.io/crates/flashraw) instead, which re-exports what applications need.

* Documentation: <https://docs.rs/flashraw-core>
* Project overview and architecture: [README](https://github.com/ybouane/flashraw/blob/main/README.md), [architecture](https://github.com/ybouane/flashraw/blob/main/docs/architecture.md)
* Licence: MIT OR Apache-2.0 (`LICENSE-MIT`, `LICENSE-APACHE`)

<!-- flashraw-outro -->
## Built with FlashRaw

- **[LightFlow](https://lightflow.dev)**: professional photo correction and colour grading, running locally in your browser with WebGPU.
- **[ImageFlow](https://imageflow.dev)**: a Photoshop-class image editor in your browser.

Official website: **[flashraw.ybouane.com](https://flashraw.ybouane.com)** · Created by [@ybouane](https://x.com/ybouane) · Licence: MIT OR Apache-2.0
<!-- /flashraw-outro -->
