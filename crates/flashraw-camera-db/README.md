# FlashRaw camera database

<!-- flashraw-intro -->
<p align="center"><a href="https://flashraw.ybouane.com"><img src="https://raw.githubusercontent.com/ybouane/flashraw/main/branding/readme-banner.png" alt="FlashRaw: the fast, safe camera RAW decoder" width="100%"></a></p>

**[FlashRaw](https://flashraw.ybouane.com)** is a fast, memory-safe camera RAW decoder written in Rust, with Rust, C, C++, Python and JavaScript (WebAssembly) bindings. It decodes about 40 RAW formats (CR2, CR3, NEF, ARW, RAF, DNG, RW2, ORF, PEF and more) with SIMD kernels, multithreading and an optional GPU backend, converts to display-ready RGB with ICC colour management, and runs in the browser.

**Official website and live demo: [flashraw.ybouane.com](https://flashraw.ybouane.com)** (it decodes your file locally in the browser) · [Source on GitHub](https://github.com/ybouane/flashraw) · Follow [@ybouane on X](https://x.com/ybouane)
<!-- /flashraw-intro -->

`data/cameras.json` is the single camera database for the whole library. It is compiled
into static Rust tables by `crates/flashraw-camera-db/build.rs`; never edit generated
Rust. Edit the JSON (or normalise it with `flashraw camera-db generate`). See
[docs/camera-db.md](../../docs/camera-db.md) for the format and tooling.

## Verification

`tools/camera_db/rederive.py` checks the entries against FlashRaw's decodes of the sample files
(the CC0 raw.pixls.us corpus and real-world files) and regenerates the fields the files state or
show: names, CFA, active area, masked areas, black and white levels, image areas, and colour
matrices from camera-written DNG files. Values the files cannot supply (colour matrices of
bodies that write no DNG, decoder parameters, entries without sample files) are kept as stored.
`flashraw camera-db coverage` prints how many entries per make carry each kind of data, and
[docs/camera-db-audit.md](../../docs/camera-db-audit.md) lists, per field group and make, what the
files reproduce, contradict or cannot derive.

Each entry's `verification` holds the number of sample files it was confirmed against, the largest
sample value observed in them, and notes.

<!-- flashraw-outro -->
## Built with FlashRaw

- **[LightFlow](https://lightflow.dev)**: professional photo correction and colour grading, running locally in your browser with WebGPU.
- **[ImageFlow](https://imageflow.dev)**: a Photoshop-class image editor in your browser.

Official website: **[flashraw.ybouane.com](https://flashraw.ybouane.com)** · Created by [@ybouane](https://x.com/ybouane) · Licence: MIT OR Apache-2.0
<!-- /flashraw-outro -->
