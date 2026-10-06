<div align="center">

# ⚡ FlashRaw

**The fast, safe camera RAW decoder.**

Rust core · C, C++, Python and JavaScript bindings · SIMD, all cores and the GPU · runs in the browser

[![CI](https://github.com/ybouane/flashraw/actions/workflows/ci.yml/badge.svg)](https://github.com/ybouane/flashraw/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#-license)
[![MSRV 1.85](https://img.shields.io/badge/rust-1.85%2B-orange.svg)](#api-stability)

</div>

FlashRaw opens a camera RAW file and gives you the sensor data plus everything needed to
interpret it (CFA layout, black and white levels, white balance, colour matrices, crops,
orientation), the embedded previews, or a display-ready RGBA image. It handles every make and
model on raw.pixls.us, it is written in memory-safe Rust, and on the same machine it decodes
**8–11× faster than LibRaw** and **1.4–2.3× faster than RawSpeed**, and develops to RGB
**8–15× faster than LibRaw** (median over 62 popular cameras).

| Median speedup of FlashRaw, 62 popular cameras, all cores | 🐧 Linux · Ryzen 7 9700X | 🍎 macOS · Apple M1 Ultra |
|---|---:|---:|
| ⚡ RAW decode vs **LibRaw** | **8.26×** | **11.2×** |
| ⚡ RAW decode vs **RawSpeed** | **2.26×** | **1.44×** |
| 🎨 RAW → display RGB vs **LibRaw** | **8.42×** | **15.1×** |

<sub>Same files, same machine, each library built for speed (LibRaw with OpenMP, RawSpeed
with OpenMP and `-march=native`); medians of per-file speedups. Full method, per-camera and
per-format numbers, and the cases where FlashRaw is *not* the fastest:
[docs/benchmarks.md](docs/benchmarks.md).</sub>

## ✨ Why FlashRaw

<table>
<tr>
<td width="33%" valign="top">

### ⚡ Speed

- **Every** decoder is multithreaded, even single-stream codecs (lossless JPEG, Nikon,
  Pentax, Hasselblad, Panasonic v8, Sony ARW1), by speculative parallel decoding
- Explicit SIMD: SSE4.1 / AVX2 / AVX-512, NEON and WASM SIMD128, picked at run time
- 64-bit bit pumps, table-driven Huffman, zero-copy parsing, memory-mapped input
- Fused single-pass RAW → RGB, on the CPU or the **GPU**
- Region and tile decodes, zero-copy embedded previews: no full decode when you
  don't need one

</td>
<td width="33%" valign="top">

### 🛡️ Security

- **Memory-safe Rust**: parsers that read untrusted files cannot overflow buffers or
  use freed memory; the few `unsafe` blocks are audited and justified
- Bounds-checked parsing, checked offset arithmetic, IFD loop and depth tracking
- Allocation **limits** checked before allocating
- **10 fuzz targets** on every PR and nightly; crash inputs replayed forever
- Malformed-file and truncation suites, and an explicit **truncation policy**:
  never silent garbage

</td>
<td width="33%" valign="top">

### 📷 Support

- **~40 RAW formats**, every make and model on raw.pixls.us: 1,980 of 2,016 corpus
  files decode, with pinned output hashes
- **CR3** incl. C-RAW, **JPEG XL DNG**, lossy and deflate DNG, GoPro GPR, Sigma X3F,
  Fuji compressed, Panasonic v4–v8, IIQ, 3FR/FFF, …
- **GPU**: Metal, Vulkan, DX12 and **WebGPU**
- **Everywhere**: Rust, C, C++, Python, npm (browsers, Web Workers, Node)
- Linux, macOS, Windows · x86-64 and ARM64 · WebAssembly

</td>
</tr>
</table>

## 🏁 Benchmarks: popular cameras

Wall time in milliseconds on an Apple M1 Ultra (20 cores, macOS), median of 7 runs, lower is
better, **bold** = fastest. *Decode* = open + full sensor decode to 16-bit samples. *RAW → RGB*
= decode + demosaic + white balance + colour conversion to an 8-bit sRGB image (RawSpeed has no
RGB pipeline). ❌ = not supported.

| Camera | Format · codec | MP | Decode<br>⚡ FlashRaw | Decode<br>LibRaw | Decode<br>RawSpeed | RAW → RGB<br>⚡ FlashRaw | RAW → RGB<br>LibRaw |
|---|---|---:|---:|---:|---:|---:|---:|
| Canon EOS R5 (RAW) | CR3 · canon-crx | 47 | **80.4** | 137 | ❌ | **110** | 732 |
| Canon EOS R6 Mark II (RAW) | CR3 · canon-crx | 25 | **45.5** | 76.4 | ❌ | **62.2** | 406 |
| Canon EOS 5D Mark IV (RAW) | CR2 · lossless-jpeg | 31 | **19.3** | 424 | 107 | **39.4** | 834 |
| Nikon Z8 (14-bit lossless) | NEF · nikon-huffman | 46 | **43.9** | 540 | 222 | **73.8** | 1145 |
| Nikon Z6III (lossless) | NEF · nikon-huffman | 24 | **24.9** | 279 | 108 | **41.6** | 622 |
| Nikon D850 (14-bit lossless) | NEF · nikon-huffman | 46 | **45.0** | 485 | 170 | **75.8** | 1081 |
| Sony A7 IV (compressed) | ARW · sony-arw2 | 33 | **5.81** | 84.5 | 8.23 | **28.3** | 533 |
| Sony A7 IV (lossless) | ARW · sony-lossless | 37 | 18.2 | 417 | **11.2** | **41.6** | 860 |
| Sony A7R V (lossless) | ARW · sony-lossless | 65 | 32.4 | 759 | **22.9** | **71.6** | 1559 |
| Sony A1 (compressed) | ARW · sony-arw2 | 50 | **8.15** | 128 | 13.3 | **41.7** | 791 |
| Fujifilm X-T5 (lossless compressed) | RAF · fuji-compressed | 41 | **35.3** | 150 | 44.2 | **74.7** | 688 |
| Fujifilm X-H2 (compressed) | RAF · fuji-compressed | 41 | **46.3** | 175 | ❌ | **94.0** | 879 |
| Fujifilm GFX100 II (lossless compressed) | RAF · fuji-compressed | 103 | **58.7** | 264 | 71.3 | **124** | 1589 |
| Panasonic Lumix S5II | RW2 · panasonic-v8 | 24 | **14.2** | 130 | 62.8 | **30.7** | 471 |
| Panasonic Lumix GH6 | RW2 · panasonic-v8 | 25 | **14.9** | 131 | 63.7 | **32.5** | 491 |
| OM System OM-1 Mark II | ORF · olympus | 20 | **113** | 384 | 159 | **129** | 674 |
| Pentax K-3 Mark III | PEF · pentax-huffman | 27 | **29.4** | 195 | 83.3 | **47.1** | 557 |
| Ricoh GR III | DNG · lossless-jpeg | 25 | **18.2** | 312 | 94.1 | **34.5** | 649 |
| Leica Q2 | DNG · uncompressed | 47 | **4.22** | 287 | 48.9 | **35.5** | 910 |
| Leica M10-R | DNG · lossless-jpeg | 41 | **28.5** | 543 | 195 | **55.8** | 1092 |
| Hasselblad X2D 100C (compressed) | FFF · hasselblad | 105 | **75.5** | 991 | 442 | **147** | 2316 |
| Phase One IQ4 150MP (IIQ L) | IIQ · phaseone | 154 | **40.4** | 1010 | ❌ | **139** | 4360 |
| Sigma fp L | DNG · lossless-jpeg | 60 | 27.6 | 752 | **21.1** | **85.1** | 1544 |
| Apple iPhone 12 Pro (ProRAW) | DNG · lossless-jpeg | 12 | **15.2** | 461 | ❌ | **23.8** | 587 |
| Google Pixel 8 Pro | DNG · lossless-jpeg | 50 | 21.8 | 636 | **16.2** | **71.3** | 1422 |
| Samsung Galaxy S23 Ultra | DNG · lossless-jpeg | 50 | **128** | 1864 | 535 | **196** | 2150 |
| Samsung Galaxy S22 (JPEG XL) | DNG · jpeg-xl | 12 | **271** | ❌ | ❌ | **293** | ❌ |
| DJI Phantom 4 Pro | DNG · uncompressed | 20 | 1.21 | 16.6 | **1.15** | **31.9** | 294 |
| GoPro HERO11 Black | DNG · gopro-vc5 | 27 | 38.5 | ❌ | **18.2** | **66.3** | ❌ |

The full list (62 bodies, phones, drones and action cameras), the same table on Linux x86-64,
single-thread results, GPU timings and peak memory are in
[docs/benchmarks.md](docs/benchmarks.md). Reproduce everything with
[`tools/bench/compare.py`](tools/bench/compare.py).

## 🆚 Feature comparison

| | ⚡ FlashRaw | LibRaw 0.22 | RawSpeed |
|---|:---:|:---:|:---:|
| Language | 🦀 Rust | C++ | C++ |
| Memory-safe parsing of untrusted files | ✅ | ❌ | ❌ |
| License | MIT / Apache-2.0 | LGPL-2.1 / CDDL-1.0 | LGPL-2.1 |
| Benchmark set: 159 files covering every format and codec | ✅ 159 | ⚠️ 156 | ⚠️ 126 |
| Canon CR3 (incl. C-RAW) | ✅ | ✅ | ❌ |
| Fujifilm lossy compressed RAF | ✅ | ✅ | ❌ |
| JPEG XL DNG (DNG 1.7, e.g. Samsung Galaxy) | ✅ | ⚠️ Adobe DNG SDK build only | ❌ |
| Lossy (JPEG) DNG | ✅ built in | ⚠️ needs libjpeg | ⚠️ needs libjpeg |
| GoPro GPR (VC-5) | ✅ | ⚠️ GPR SDK build only | ✅ |
| Sigma X3F | ✅ | ✅ | ❌ |
| Nikon High Efficiency (TicoRAW) | ❌ | ❌ | ❌ |
| DNG opcode lists | ✅ all radiometric opcodes, per stage | ⚠️ Adobe DNG SDK build only | ⚠️ stage 1 subset |
| RAW → RGB pipeline | ✅ bilinear, half-size (CPU or GPU) | ✅ many demosaics | ❌ raw data only |
| Multithreaded decoding | ✅ every codec | ⚠️ 3 codecs (OpenMP build) | ⚠️ tiled/sliced codecs (OpenMP) |
| Explicit SIMD kernels | ✅ SSE4.1/AVX2/AVX-512, NEON, WASM | ❌ | ⚠️ one SSE2 routine |
| GPU acceleration | ✅ Metal, Vulkan, DX12, WebGPU | ❌ | ❌ |
| Browser / WebAssembly package | ✅ official npm package | ⚠️ third-party ports | ❌ |
| Region and tile decoding | ✅ | ❌ | ❌ |
| Zero-copy embedded previews | ✅ | ⚠️ copied | ❌ no previews |
| Continuous fuzzing | ✅ in-tree, every PR | ✅ OSS-Fuzz | ✅ in-tree |
| C API | ✅ `flashraw.h` | ✅ | ❌ |
| C++ API | ✅ `flashraw.hpp` | ✅ | ✅ |
| Python | ✅ `pip install flashraw` (NumPy, zero-copy) | ⚠️ third-party (rawpy) | ❌ |
| Rust | ✅ native | ⚠️ third-party bindings | ❌ |
| LibRaw-compatible C API (recompile and relink) | ✅ subset | – | ❌ |

<sub>Checked against LibRaw 0.22.0 and RawSpeed (develop, c835b05) sources and builds. "Benchmark
set" counts files each library decoded in our runs, with LibRaw and RawSpeed built without
optional SDKs (libjpeg, Adobe DNG SDK, GPR SDK). Nikon HE is a proprietary codec that no open
decoder supports.</sub>

## 🛡️ Security

RAW files are untrusted input: they come from the web, from email, from memory cards. FlashRaw
is built for that.

- ✅ **Memory-safe Rust.** Parsing and decoding are safe Rust. `unsafe` is confined to SIMD
  intrinsics, zeroed allocation, a disjoint-tile writer, the input self-reference, the WASM
  clock hook and the mmap call, each with a `SAFETY:` justification and tests
  ([architecture](docs/architecture.md)).
- ✅ **Fuzzed continuously.** Ten cargo-fuzz targets (the whole open/decode pipeline over every
  format under both corruption policies, TIFF and BMFF parsers, bit pumps against a model,
  Huffman, lossless JPEG, packed unpack, deflate/float, DCT, DNG opcodes) run on every PR and
  for 15 minutes each nightly. Every crash input is kept and replayed.
- ✅ **Malformed-file suite.** Truncation at every cut point, invalid offsets and counts,
  recursive IFDs, overflowing dimensions, broken Huffman tables, corrupted entropy data,
  garbage behind every magic number, and mutations of a real file of every extension: each
  must finish without panicking within a time bound ([testing](docs/testing.md)).
- ✅ **Resource limits.** Image sizes are checked against `Limits` (samples, dimensions)
  *before* anything is allocated, so a 20-byte file cannot ask for 64 GB.
- ✅ **No silent garbage.** Sensor data that runs past the end of the file is recovered with
  structured warnings or rejected, as you choose (see
  [truncated and corrupt sensor data](#truncated-and-corrupt-sensor-data)).
- ✅ **Sandboxable.** The npm package runs the decoder inside WebAssembly: a decoder bug cannot
  corrupt the host process.

## 📷 Supported formats, cameras and platforms

| | |
|---|---|
| **Formats** | DNG (uncompressed, lossless JPEG, deflate int/float, lossy JPEG, **JPEG XL**, GoPro VC-5, Blackmagic), CR2 (incl. sRAW/mRAW), **CR3** (CRX and C-RAW), CRW, NEF/NRW, ARW/SR2/SRF/ARQ, RAF (incl. compressed X-Trans), RW2/RWL (Panasonic v4–v8), ORF/ORI, PEF, SRW, MRW, IIQ, MOS, 3FR/FFF, KDC/DCR/DCS, ERF, MEF, X3F, ARI, headerless and CHDK raws |
| **Cameras** | every make and model on raw.pixls.us (1,980 of 2,016 files decode), 1,000+ models in the built-in camera database, and DNG from any camera, phone or drone |
| **Not supported** | Nikon High Efficiency (proprietary TicoRAW codec) and a few rare variants: see [formats](docs/formats.md) |
| **Outputs** | raw `u16` / `f32`, normalised `f32`, linear RGB `f32`, display RGBA8 (bilinear or half-size demosaic, as-shot white balance, colour matrix, sRGB, orientation), GPU-resident textures |
| **GPU** | wgpu: Metal, Vulkan and DX12 natively, WebGPU in browsers; `Backend::Auto` uses the GPU only where it measures a win ([GPU](docs/gpu.md)) |
| **Languages** | Rust, C, C++, Python, JavaScript/TypeScript, plus a LibRaw-compatible C API |
| **Platforms** | Linux, macOS, Windows (x86-64, ARM64); WebAssembly in Chrome, Edge, Firefox and Safari, Web Workers and Node.js, multithreaded where the page is cross-origin isolated |

Support is tracked per format *and mode* (`verified`, `partial`, `experimental`,
`unsupported`): see [docs/formats.md](docs/formats.md) or run `flashraw supported`.

## 🚀 Quick start

### 🦀 Rust

```toml
[dependencies]
flashraw = "0.1"
```

```rust
use flashraw::{FlashRaw, DecodeOptions, Backend, OutputFormat};

fn main() -> flashraw::Result<()> {
    let raw = FlashRaw::open_path("photo.NEF")?;          // memory-mapped, headers only

    let m = raw.metadata();
    println!("{} {}x{} ISO {:?}", m.camera(), m.width, m.height, m.exposure.iso);

    // Embedded JPEG preview, zero-copy, without decoding the RAW payload:
    if let Some(p) = raw.preview() {
        std::fs::write("preview.jpg", raw.preview_bytes(p)?)?;
    }

    // Sensor data:
    let frame = raw.decode(DecodeOptions {
        backend: Backend::Auto,
        output: OutputFormat::RawU16,
        ..Default::default()
    })?;
    println!(
        "{}x{} CFA {:?} black {:?} white {:?} via {}",
        frame.width(), frame.height(), frame.cfa, frame.black_level.values,
        frame.white_level.values, frame.stats().backend.decode
    );

    // Display-ready RGBA8 (bilinear demosaic, as-shot WB, colour matrix, sRGB, orientation):
    let rgba = raw.decode(DecodeOptions { output: OutputFormat::Rgba8, ..Default::default() })?;
    Ok(())
}
```

Input sources: `FlashRaw::open(&[u8])` (zero-copy borrow), `open(Vec<u8>)`,
`open(Arc<[u8]>)`, `open(memmap2::Mmap)`, `open_path(path)`, `from_reader(reader)`.

Other entry points: `decode_region(rect, opts)`, `decode_tile(index, opts)`,
`units()` (independently decodable tiles/strips with byte ranges),
`capabilities()`, `previews()` / `thumbnail()` / `decode_preview_rgb()`,
`flashraw::read_metadata(bytes)`, `flashraw::supports(bytes)`.

Output formats: `RawU16` (default), `RawF32`, `NormalizedF32` (black-subtracted,
white-scaled), `LinearRgbF32`, `Rgba8`. Statistics (`frame.stats()`) report
per-stage timings and the backend that ran.

### 🐍 Python

```sh
pip install flashraw
```

```python
import flashraw

with flashraw.open("photo.NEF") as raw:
    print(raw.metadata.camera.name, raw.metadata.exposure.iso)
    sensor = raw.decode().array                  # numpy uint16 (h, w), zero-copy
    rgba = raw.decode(output="rgba8").array      # numpy uint8 (h, w, 4), display-ready
```

Decoding releases the GIL, so Python threads decode files in parallel. See
[docs/python.md](docs/python.md).

### 🌐 Browser and Node.js (npm)

```js
import { FlashRaw } from "flashraw";

const buffer = await (await fetch("/photo.CR3")).arrayBuffer();
const raw = await FlashRaw.open(buffer);

console.log(raw.metadata.camera, raw.metadata.width, raw.metadata.height);
const preview = await raw.preview();                      // embedded JPEG, instantly
const frame = await raw.decode({ backend: "auto", output: "raw-u16" });
console.log(frame.width, frame.height, frame.cfa, frame.backend, frame.timings);

const img = await raw.decode({ output: "rgba8" });
ctx.putImageData(new ImageData(img.data, img.width, img.height), 0, 0);
raw.close();
```

See [npm/README.md](npm/README.md) for Node, React/Vite, Web Workers, metadata-only,
preview extraction, GPU and CPU fallback examples, and [docs/wasm.md](docs/wasm.md).

### 🔧 C

```c
#include <flashraw.h>

flashraw_file *f = NULL;
if (flashraw_open_path("photo.NEF", NULL, &f) != FLASHRAW_OK) {
    fprintf(stderr, "%s\n", flashraw_last_error_message());
    return 1;
}
flashraw_decode_options o;
flashraw_decode_options_init(&o);
o.output = FLASHRAW_OUTPUT_RGBA8;
flashraw_frame *fr = NULL;
if (flashraw_decode(f, &o, &fr) == FLASHRAW_OK) {
    const uint8_t *rgba = flashraw_frame_data(fr);  /* width x height x 4 bytes */
    flashraw_frame_free(fr);
}
flashraw_file_free(f);
```

### ➕ C++

```cpp
#include <flashraw.hpp>

flashraw::file raw = flashraw::file::open("photo.CR3");   // throws flashraw::error
flashraw::decode_options o;
o.output = flashraw::output_format::linear_rgb_f32;
flashraw::frame img = raw.decode(o);
flashraw::span<const float> rgb = img.f32();             // no copy
```

Build with `cargo build --release -p flashraw-capi` or install with CMake (`find_package(flashraw)`,
pkg-config). See [docs/c-api.md](docs/c-api.md).

### 🔁 Coming from LibRaw?

`flashraw-libraw-compat` implements the commonly used part of the LibRaw C API
(`libraw_init`, `libraw_open_file`, `libraw_unpack`, `libraw_dcraw_process`,
`libraw_dcraw_make_mem_image`, …) on top of FlashRaw. Keep your `#include <libraw/libraw.h>`,
recompile against its headers and link `-lraw` from FlashRaw. See
[docs/libraw-compat.md](docs/libraw-compat.md) for the supported subset and deviations.

### 💻 CLI

```sh
cargo install --path crates/flashraw-cli

flashraw info photo.CR3                     # summary (--json for machine output)
flashraw dump-metadata photo.ARW            # everything, as JSON
flashraw decode photo.NEF out.raw           # raw u16 samples (.pgm/.tiff also supported)
flashraw decode photo.RAF out.jpg --output rgba8 --profile
flashraw decode photo.DNG tile.pgm --tile 12
flashraw decode photo.DNG crop.pgm --region 1000,800,512,512
flashraw preview photo.CR2 preview.jpg      # embedded full-size JPEG
flashraw thumbnail photo.CR2 thumb.jpg
flashraw benchmark photo.NEF --iterations 10 --threads 8
flashraw verify photo.ORF --json            # geometry, levels and full-buffer hashes
flashraw verify cut.DNG --json --strict     # fail on truncated data (default: recover + warnings)
flashraw supported                          # format/mode support matrix
flashraw camera-db validate|generate|compare|stats
```

Options: `--backend auto|cpu|simd|gpu`, `--output raw-u16|raw-f32|normalized|linear-rgb|rgba8`,
`--region x,y,w,h`, `--tile N`, `--threads N`, `--demosaic bilinear|half`,
`--profile`, `--json`.

```
$ flashraw decode photo.DNG out.jpg --output rgba8 --profile
Container parse:             0.05 ms
Pixel unpack:                4.75 ms
Post-process:               28.82 ms
Total:                      34.85 ms

Backend:
  decode: CPU SSE4.1 x16 threads
  postprocess: CPU x16
```

## 📚 Reference

### Truncated and corrupt sensor data

A strip, tile or other sensor-data range that runs past the end of the file is never
silently zero-filled. `DecodeOptions::corruption_policy` decides what happens:

- `CorruptionPolicy::Recover` (default): every complete sample is decoded, missing ones
  are zero, and the frame says so: `frame.is_incomplete()` and `frame.warnings()`, each a
  `DecodeWarning` with the unit (strip/tile index), the bytes expected and available and
  the samples filled ("recovered truncated strip 17: 48291 / 131072 bytes available;
  41390 missing samples were zero-filled").
- `CorruptionPolicy::Strict`: the decode fails with `Error::CorruptFile`; `e.truncation()`
  (or `e.corrupt_reason()`) gives the same details ("truncated image data: strip 17
  expected 131072 bytes at offset 24576000, but only 48291 bytes remain before EOF").
  `e.kind()` stays `"CorruptFile"`.

Only the data a decode needs is checked: metadata, previews and region decodes that do
not touch the missing data work normally. Short data that still holds every sample is
complete: the last row needs no padding to the row stride, and a declared length beyond
the last needed sample may run past the end of the file. Recovery covers uncompressed
and bit-packed payloads in every format and compressed DNG strips/tiles (a unit that is
truncated or fails to decode is zero-filled with a warning), and Nikon and Pentax Huffman
and Samsung SRW streams cut short by the end of the file (the rows decoded from real data
are kept). Other compressed streams (one entropy-coded stream per image) fail under both
policies; when their declared range runs past the end of the file the error carries the
same truncation details. A file cut inside its directory structure, before the raw image's
IFD, fails with `Error::CorruptFile`: a thumbnail or preview is never decoded in place of
the sensor image.

### GPU

Build with `--features gpu`. Packed/uncompressed payloads are unpacked on the GPU;
black subtraction, demosaic, white balance, colour conversion and sRGB encoding run
as WGSL compute shaders shared by native (Vulkan/Metal/DX12) and browser (WebGPU)
builds; results can stay GPU-resident. `Backend::Auto` only picks the GPU when its
measured cost model predicts a win. See [docs/gpu.md](docs/gpu.md).

### API stability

FlashRaw follows semver. The public API is everything documented at the crate root:
`FlashRaw`, `Source`, `DecodeOptions` and its enums, `RawFrame`, the metadata types,
`Error`, and the `gpu` module (feature `gpu`).

- Enums that can gain variants (formats, compressions, outputs, backends, errors,
  stages, preview kinds, ...) are `#[non_exhaustive]`: match them with a wildcard arm.
- Result and metadata structs (`RawFrame`, `RawMetadata`, `FrameInfo`,
  `DecodeStatistics`, `Limits`, ...) are `#[non_exhaustive]`: read their fields; build
  `Limits` with `Limits::default().with_max_samples(..)`.
- Build `DecodeOptions` with `..Default::default()` (new options are added as fields
  with backwards-compatible defaults, e.g. `corruption_policy`).
- `Error::CorruptFile` and its structured cause (`CorruptReason`, `Truncation`,
  `DataUnit`) and `DecodeWarning` are `#[non_exhaustive]`; read them through their
  fields and `Error::truncation()` / `Error::corrupt_reason()`.
- `Backend`, `OutputFormat`, `Demosaic` and `CorruptionPolicy` implement
  `FromStr`/`Display` with the names the CLI and the JS bindings use.
- Minimum supported Rust version: 1.85 for the library with default features and
  `serde`; 1.87 with `gpu` (and for the CLI and WASM crates); 1.88 with `jpegxl`.
  Raising it is a minor-version change.
- Hidden items (`low_level`, `process`, `decode_raw_image`) expose internals for
  tooling and carry no stability guarantee.

### Cargo features

| feature | default | effect |
|---|---|---|
| `rayon` | yes | multithreaded decoding |
| `simd` | yes | explicit SIMD kernels (runtime dispatch on x86-64) |
| `mmap` | yes | memory-mapped `open_path` |
| `gpu` | no | wgpu backend (Vulkan / Metal / DX12 / WebGPU) |
| `jpegxl` | no | JPEG XL compressed DNG (DNG 1.7) |
| `serde` | no | `Serialize` for metadata types |

## 📖 Documentation

[Architecture](docs/architecture.md) · [Formats](docs/formats.md) ·
[Benchmarks](docs/benchmarks.md) · [GPU](docs/gpu.md) · [WASM](docs/wasm.md) ·
[C and C++ API](docs/c-api.md) · [LibRaw-compatible API](docs/libraw-compat.md) ·
[Python](docs/python.md) · [Testing](docs/testing.md) · [Profiling](docs/profiling.md) ·
[Camera DB](docs/camera-db.md) · [Contributing](docs/contributing.md) ·
[References](docs/references.md)

## 📄 License

Code and data (including `camera-db/`): MIT OR Apache-2.0, at your option. See
[LICENSE-MIT](LICENSE-MIT) and [LICENSE-APACHE](LICENSE-APACHE).
