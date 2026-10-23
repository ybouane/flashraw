# Building FlashRaw: dependencies and features

FlashRaw is a Cargo workspace. Every optional capability is a Cargo feature, so a build that
does not need something can leave it out: smaller binaries, fewer dependencies, faster
compiles. This page lists what each crate pulls in and how to build exactly what you need.

## Third-party dependencies

The RAW parsers, the entropy decoders (lossless JPEG, Canon CRX, Sony, Nikon, Fuji, Panasonic,
Phase One, ...), demosaicing, colour processing and the camera database are FlashRaw's own code.
The third-party crates below are used only for the jobs listed. All are pure Rust except
`libc` (system bindings) and, with the `gpu` feature, the platform graphics APIs behind wgpu.

| Crate | Used for | Needed by | Switch |
|---|---|---|---|
| `thiserror` | `Error` derive | `flashraw-core` | always (compile-time only: a proc-macro) |
| `zune-jpeg` | baseline/progressive JPEG (embedded previews, lossy DNG tiles) | `flashraw-codecs` | always |
| `zune-inflate`, `zlib-rs` | zlib/deflate (compressed DNG, float DNG) | `flashraw-codecs` | always |
| `rayon` | multithreading | `flashraw-core`, `-codecs`, `-formats`, `flashraw` | feature `rayon` |
| `memmap2` | memory-mapped file input | `flashraw` | feature `mmap` |
| `moxcms` (+ `pxfm`, `num-traits`) | arbitrary ICC output profiles | `flashraw` | feature `icc` |
| `jxl-oxide` | JPEG XL compressed DNG (DNG 1.7) | `flashraw-codecs` | feature `jpegxl` |
| `wgpu`, `bytemuck` | GPU backend | `flashraw-gpu` | feature `gpu` |
| `serde` | metadata types derive Serialize/Deserialize | `flashraw-core` | feature `serde` |
| `serde_json` | JSON output | CLI, C API, Python, WASM bindings | the binding crates |
| `clap`, `jpeg-encoder` | command line, JPEG writing | `flashraw-cli` | the CLI only |
| `pyo3`, `numpy` | Python module | `flashraw-python` | the Python wheel only |
| `wasm-bindgen` | JS glue | `flashraw-wasm` | the WASM build only |

The `flashraw` library crate with `--no-default-features` depends on the `flashraw-*` workspace
crates, `thiserror`, `zune-jpeg`, `zune-inflate` and `zlib-rs`, plus their small transitive
dependencies (`zune-core`, `simd-adler32`, `libc`).

### Why `moxcms`

Built-in colour spaces (sRGB, Display P3, Adobe RGB, ProPhoto, Rec. 2020) and their embedded
profiles are FlashRaw's own code and never need it. `moxcms` is used only to read and apply an
arbitrary `.icc` file (`output_profile`), where profiles may contain lookup tables, several
tag types and two profile versions. Re-implementing that is possible but is a large amount of
code to keep correct; it is isolated behind the `icc` feature so you can drop it.

Licences: all dependencies are permissive (MIT, Apache-2.0, BSD-3-Clause, Zlib). `moxcms` and
`pxfm` are BSD-3-Clause OR Apache-2.0.

## Features of the `flashraw` crate

| Feature | Default | Effect |
|---|---|---|
| `rayon` | yes | Multithreaded decoding and development. Without it everything runs on the calling thread. |
| `simd` | yes | SIMD unpack kernels: SSSE3/SSE4.1 on x86-64 (runtime dispatch), NEON on aarch64, SIMD128 on WebAssembly (compile time). Only packed-bit unpacking has explicit kernels; the rest of the pipeline relies on compiler auto-vectorisation. Scalar code is always compiled. |
| `mmap` | yes | `FlashRaw::open_path` memory-maps the file. Without it the file is read into memory. |
| `icc` | yes | `DecodeOptions::output_profile`: arbitrary ICC profiles. Without it a profile returns `InvalidArgument("built without ICC support")`; the built-in colour spaces and `icc_profile()` still work. |
| `jpegxl` | no | JPEG XL compressed DNG. Needs Rust 1.88. |
| `gpu` | no | GPU backend (Metal, Vulkan, DX12, WebGPU) through wgpu. |
| `serde` | no | `Serialize`/`Deserialize` on metadata types. |

Rust library users:

```toml
# everything on by default
flashraw = "0.1"
# smallest: single-threaded, no mmap, no SIMD, no ICC files
flashraw = { version = "0.1", default-features = false }
# pick à la carte
flashraw = { version = "0.1", default-features = false, features = ["rayon", "simd"] }
```

## The other builds

| Build | Command | Features |
|---|---|---|
| CLI | `cargo build --release -p flashraw-cli` | always rayon, simd, mmap, icc, serde. Optional: `--features gpu,jpegxl`. |
| C library | `cargo build --release -p flashraw-capi` | default `icc`; optional `gpu`, `jpegxl`. Drop ICC: `--no-default-features`. |
| Python | `maturin build --release` in `crates/flashraw-python` | default `icc`; optional `gpu`, `jpegxl`. Drop ICC: `--no-default-features --features extension-module`. |
| npm / WASM | `cd npm && npm run build` | six variants (see [wasm.md](wasm.md)); `icc` is on in all of them. |

### A smaller WASM build

`icc` costs about 88 KiB (brotli) per WASM binary, the built-in colour spaces about 8 KiB
(see the size table in [wasm.md](wasm.md)). To build the package without ICC files:

```sh
cd npm
FLASHRAW_NO_ICC=1 npm run build
```

`--variants simd,threads` builds only some binaries, and `--no-opt` skips `wasm-opt`.
Variants you do not ship are never downloaded: the loader fetches a single `.wasm`, plus the
JPEG XL one only when a file needs it. If you build only `simd`, the loader falls back to it
for every page.

## Minimal-dependency checklist

- No threads (embedded targets, single-core WASM): `default-features = false`.
- No `unsafe` mmap: leave out `mmap`.
- No colour management engine: leave out `icc`.
- Smaller binary: build with `--profile wasm-release` or set `opt-level = "s"` in your profile;
  leave out `jpegxl` and `gpu` (both off by default).
- Check what you ship: `cargo tree -p flashraw --no-default-features -e normal`.
