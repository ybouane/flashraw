# flashraw

<!-- flashraw-intro -->
<p align="center"><a href="https://flashraw.ybouane.com"><img src="https://raw.githubusercontent.com/ybouane/flashraw/main/branding/readme-banner.png" alt="FlashRaw: the fast, safe camera RAW decoder" width="100%"></a></p>

**[FlashRaw](https://flashraw.ybouane.com)** is a fast, memory-safe camera RAW decoder written in Rust, with Rust, C, C++, Python and JavaScript (WebAssembly) bindings. It decodes about 40 RAW formats (CR2, CR3, NEF, ARW, RAF, DNG, RW2, ORF, PEF and more) with SIMD kernels, multithreading and an optional GPU backend, converts to display-ready RGB with ICC colour management, and runs in the browser.

**Official website and live demo: [flashraw.ybouane.com](https://flashraw.ybouane.com)** (it decodes your file locally in the browser) · [Source on GitHub](https://github.com/ybouane/flashraw) · Follow [@ybouane on X](https://x.com/ybouane)
<!-- /flashraw-intro -->

Fast, safe camera RAW decoding in Rust: DNG, CR2, CR3, NEF, ARW, RAF, RW2, ORF, PEF and more, with
bounds-checked parsing, configurable `Limits` for untrusted files, multithreaded CPU decoding, an
optional GPU backend and the same API compiled to WebAssembly, Python and C.

```toml
[dependencies]
flashraw = "0.1"
```

```rust,no_run
use flashraw::{DecodeOptions, FlashRaw, OutputFormat};

fn main() -> flashraw::Result<()> {
    let raw = FlashRaw::open_path("photo.CR3")?;                 // headers only, memory-mapped
    println!("{} {}x{}", raw.metadata().camera(), raw.metadata().width, raw.metadata().height);
    let frame = raw.decode(DecodeOptions { output: OutputFormat::Rgba8, ..Default::default() })?;
    println!("{} bytes of RGBA8", frame.as_u8().map_or(0, |d| d.len()));
    Ok(())
}
```

Of the 2,016 files in the raw.pixls.us corpus, 2,008 decode (99.6%); the other 8 are valid files in
formats that are not supported yet (Light L16, Casio CAM, JPEG-in-DNG), see
[docs/formats.md](https://github.com/ybouane/flashraw/blob/main/docs/formats.md).

* API documentation: <https://docs.rs/flashraw> (feature table on the crate page)
* Guide, benchmarks, CLI, Python, JavaScript and C bindings: [README](https://github.com/ybouane/flashraw/blob/main/README.md), [docs/packages.md](https://github.com/ybouane/flashraw/blob/main/docs/packages.md)
* MSRV 1.85. Licence: MIT OR Apache-2.0.

<!-- flashraw-outro -->
## Built with FlashRaw

- **[LightFlow](https://lightflow.dev)**: professional photo correction and colour grading, running locally in your browser with WebGPU.
- **[ImageFlow](https://imageflow.dev)**: a Photoshop-class image editor in your browser.

Official website: **[flashraw.ybouane.com](https://flashraw.ybouane.com)** · Created by [@ybouane](https://x.com/ybouane) · Licence: MIT OR Apache-2.0
<!-- /flashraw-outro -->
