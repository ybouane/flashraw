# Packages

Everything FlashRaw distributes, what is in it and who needs it.

**Status of every package: unreleased.** At the time of writing nothing is published:
the GitHub repository, crates.io, PyPI and npm names below all return 404, and the install
commands do not work yet. Until the first release, build from a checkout ([building](building.md)).
Every version is `0.1.0` (the workspace version in [Cargo.toml](../Cargo.toml); the CHANGELOG entry is
"unreleased"). The release procedure is in [releasing.md](releasing.md).

If you are not sure what to install:

| You write... | Install |
|---|---|
| Rust | `flashraw` |
| Python | `flashraw` from PyPI |
| JavaScript / TypeScript (browser or Node.js) | `flashraw` from npm |
| C / C++ | the C library and header (`flashraw-capi`) |
| a shell script | the `flashraw` command-line tool (`flashraw-cli`) |

## Overview

| Package | Registry name | Version | Status | Install | Documentation |
|---|---|---|---|---|---|
| Rust library | crates.io `flashraw` | 0.1.0 | unreleased (404) | `cargo add flashraw` | [crate docs](https://docs.rs/flashraw), [README](../README.md#-quick-start) |
| Command-line tool | crates.io `flashraw-cli` (binary `flashraw`) | 0.1.0 | unreleased (404) | `cargo install flashraw-cli` | [README CLI section](../README.md#-cli) |
| npm package | npm `flashraw` | 0.1.0 | unreleased (404) | `npm install flashraw` | [npm/README.md](../npm/README.md), [wasm.md](wasm.md) |
| Python package | PyPI `flashraw` | 0.1.0 | unreleased (404) | `pip install flashraw` | [python.md](python.md), [crate README](../crates/flashraw-python/README.md) |
| C library + headers | GitHub release assets (planned), source build | 0.1.0 | unreleased | `cargo build --release -p flashraw-capi` or CMake | [c-api.md](c-api.md) |
| LibRaw-compatible shim | none (not published) | 0.1.0 | build from source | `cargo build --release -p flashraw-libraw-compat` | [libraw-compat.md](libraw-compat.md) |
| Internal crates | crates.io `flashraw-core`, `-codecs`, `-formats`, `-camera-db`, `-gpu` | 0.1.0 | unreleased (404) | pulled in by `flashraw` | below |
| Demo site | not a package | - | deployed separately | open <https://flashraw.ybouane.com> | `demo/README.md` |

All packages are licensed MIT OR Apache-2.0 and ship `LICENSE-MIT` and `LICENSE-APACHE`.

## crates.io

Seven crates are published. They are versioned together (`0.1.0`) and must be published in this order
(each depends only on the ones before it): `flashraw-camera-db`, `flashraw-core`, `flashraw-codecs`,
`flashraw-formats`, `flashraw-gpu`, `flashraw`, `flashraw-cli`. `cargo publish --workspace` computes
this order itself ([releasing.md](releasing.md)).

| Crate | Purpose | Who needs it | Install |
|---|---|---|---|
| `flashraw` | The library: open a RAW file, metadata, previews, decode to sensor data or RGB, `Limits`, CPU/SIMD/GPU backends. Re-exports the types of the internal crates. | Every Rust user | `cargo add flashraw` (features: `gpu`, `jpegxl`, `serde`; see [crate docs](https://docs.rs/flashraw)) |
| `flashraw-cli` | The `flashraw` command-line tool: `info`, `decode`, `preview`, `thumbnail`, `verify`, `supported`, `benchmark`, `camera-db`. | Anyone who wants a binary, scripts, regression hashes | `cargo install flashraw-cli` (CPU only); `cargo install flashraw-cli --features gpu,jpegxl` for the GPU backend and JPEG XL DNG |
| `flashraw-core` | Core types, bounds-checked readers, bit pumps, TIFF/BMFF parsing, parallel helpers. | Nobody directly (internal; no semver guarantee) | dependency of `flashraw` |
| `flashraw-codecs` | Compression codecs: lossless JPEG, packed unpack with SIMD kernels, deflate, vendor entropy coders. | Nobody directly (internal) | dependency of `flashraw` |
| `flashraw-formats` | Container parsers and per-vendor decoders (DNG, CR2, CR3, NEF, ARW, RAF, ...). | Nobody directly (internal) | dependency of `flashraw` |
| `flashraw-camera-db` | The camera database (`data/cameras.json`) compiled into static tables, its schema and the validation tooling (feature `tooling`). | Nobody directly, except tools that read the database | dependency of `flashraw` and `flashraw-cli` |
| `flashraw-gpu` | The optional wgpu/WebGPU compute backend, with its WGSL shaders. | Nobody directly: enable `flashraw`'s `gpu` feature | optional dependency of `flashraw` |

The internal crates are published only because `flashraw` depends on them; folding them into one
crate is not possible without a dependency cycle (the camera database is used by `flashraw-formats`,
`flashraw-cli` and `flashraw`). Not published to crates.io (`publish = false`): `flashraw-capi`,
`flashraw-wasm`, `flashraw-python`, `flashraw-libraw-compat`; they are built into the other packages below.

The minimum supported Rust version is 1.85 (1.87 with `gpu`, 1.88 with `jpegxl`). The `gpu` feature is not
built on docs.rs; its API is described in [gpu.md](gpu.md).

## Command-line tool

Binary `flashraw`, from the `flashraw-cli` crate. `cargo install flashraw-cli` builds **without** the `gpu`
and `jpegxl` features: `--backend gpu` and JPEG XL compressed DNG need
`cargo install flashraw-cli --features gpu,jpegxl`. Prebuilt binaries are not distributed. From a checkout:
`cargo install --path crates/flashraw-cli [--features gpu,jpegxl]`.

## npm package `flashraw`

For browsers (WebAssembly, Web Workers, optional WebGPU) and Node.js >= 18. Tarball contents (`files` in
`npm/package.json`): `dist/*.js`, `dist/*.d.ts`, `dist/*.wasm`, `README.md`, `LICENSE-MIT`, `LICENSE-APACHE`.
About 6.2 MB packed.

* Entry points (`exports`): `.` (Node: `dist/node.js`; browsers and bundlers: `dist/browser.js`; types
  `dist/index.d.ts`), `./worker` (worker pool, `worker.node.js` / `worker.browser.js`), `./thread-worker`,
  `./wasm/*` (the raw `.wasm` files for hosting them yourself) and `./package.json`.
* WASM variants (`dist/`; one is downloaded per page, chosen at run time):

  | File | Use |
  |---|---|
  | `flashraw_simd.wasm` | default (WASM SIMD128), 756 KiB brotli |
  | `flashraw.wasm` | fallback without SIMD, 738 KiB brotli |
  | `flashraw_simd_threads.wasm` | multithreaded (needs cross-origin isolation, COOP/COEP; always available in Node.js), 745 KiB brotli |
  | `flashraw_simd_gpu.wasm` | WebGPU backend, 812 KiB brotli |
  | `flashraw_simd_jxl.wasm`, `flashraw_simd_threads_jxl.wasm` | JPEG XL compressed DNG, loaded only when such a file is decoded |

  Sizes and the build: [wasm.md](wasm.md#sizes).
* TypeScript types are bundled (`index.d.ts`, `worker.d.ts`). Docs: [npm/README.md](../npm/README.md).

## PyPI package `flashraw`

Built with maturin from `crates/flashraw-python` (a PyO3 extension plus a typed pure-Python layer).

* **abi3** wheels: one wheel per platform for CPython 3.9 and later. Platforms in the wheel build definition
  (`.github/workflows/python.yml`, run on manual dispatch only): Linux x86-64 and aarch64 (manylinux 2_28),
  macOS x86-64 and arm64, Windows x64. Only the platforms tested locally by the maintainers (Linux x86-64,
  macOS arm64) are known to work; the others have been built by that definition but are not regularly tested.
* An sdist is built by the same definition; building from it needs a Rust toolchain (1.88+ for the JPEG XL
  support that the wheels include).
* Runtime dependency: NumPy >= 1.19. Optional extra `flashraw[pil]` (Pillow) for `Frame.to_pil()`.
* Docs: [python.md](python.md).

## C library and headers (`flashraw-capi`)

A stable C ABI plus a header-only C++17 wrapper. Not published on a registry. It is built from source:

* `cargo build --release -p flashraw-capi [--features gpu,jpegxl]` produces `libflashraw_capi.so` / `.dylib` /
  `flashraw_capi.dll` (shared) and `libflashraw_capi.a` / `flashraw_capi.lib` (static) in `target/release`.
* Headers: `crates/flashraw-capi/include/flashraw.h` (C, generated by cbindgen and checked in) and
  `flashraw.hpp` (C++17 RAII wrapper).
* CMake (`crates/flashraw-capi/CMakeLists.txt`) installs the libraries, both headers, `flashraw.pc` (pkg-config),
  the licence files and a CMake package: `find_package(flashraw)` gives the target `flashraw::flashraw`.
* Release plan: a tarball per platform (libraries, headers, licences) attached to the GitHub release. No release
  automation exists yet; the procedure is in [releasing.md](releasing.md). Usage: [c-api.md](c-api.md).

## LibRaw-compatible shim (`flashraw-libraw-compat`)

A C library named `raw` (`libraw.so` / `-lraw`) that implements a documented subset of the LibRaw C API on top
of FlashRaw, so existing C programs can relink. It is **not published**: it exports LibRaw's symbol names,
so a distribution package or crates.io upload could collide with the real library, and it is only a subset.
Build it from source (`cargo build --release -p flashraw-libraw-compat`). Docs: [libraw-compat.md](libraw-compat.md).

## Demo site

<https://flashraw.ybouane.com> (source in `demo/`): a static web page that decodes RAW files in the browser with
the npm package. It is a showcase, not a package, and is deployed independently of releases.

## Names that differ between bindings

See [api-names.md](api-names.md).
