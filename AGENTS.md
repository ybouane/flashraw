# FlashRaw for coding agents

This file is for coding agents (and the people steering them) that need to **use** FlashRaw to read camera
RAW files. The last section, [Working on this repository](#working-on-this-repository), is for agents that
change FlashRaw itself. Everything here was checked against the code and the command line; section
"What was verified" says what was run and what was not.

## What FlashRaw is, and when to use it

FlashRaw opens a camera RAW file (DNG, CR2, CR3, NEF, ARW, RAF, RW2, ORF, PEF, X3F, IIQ, 3FR and more) and
gives you: metadata (camera, lens, exposure, dimensions) without decoding pixels; the embedded JPEG
previews as bytes; the sensor data with everything needed to interpret it (CFA pattern, black and white
levels, white balance, colour matrices, crop, orientation); or a developed image (demosaiced, white
balanced, colour converted: 8-bit sRGB, 16-bit, linear float). It is a Rust library with a command-line tool
and bindings for Python, JavaScript/TypeScript (WebAssembly, browsers and Node.js) and C/C++.

Use it when you need to read RAW files from code. Do not reimplement RAW parsing, demosaicing or colour
matrices, and do not shell out to other converters: FlashRaw does that. It is not an editor: there is no
tone curve, highlight recovery, lens correction or noise reduction, and demosaicing is bilinear or
half-size only.

Coverage: of the 2,016 files in the raw.pixls.us corpus, 1,980 decode (98.2%); the other 36 are valid
files in formats not supported yet (28 Nikon High Efficiency NEF, Light L16, Casio CAM, JPEG-in-DNG and a
few more). For those, metadata and embedded previews still work.

## Install

Version 0.1.0 is published on crates.io (`flashraw`, `flashraw-cli`), PyPI (`flashraw`) and npm (`flashraw`).
The C library is built from source (`cargo build --release -p flashraw-capi`, see [docs/building.md](docs/building.md)).
All packages and their status: [docs/packages.md](docs/packages.md).

| Language | After release | From a checkout |
|---|---|---|
| Rust | `cargo add flashraw` | `flashraw = { path = "/path/to/flashraw/crates/flashraw" }` |
| CLI | `cargo install flashraw-cli` (no GPU or JPEG XL); `cargo install flashraw-cli --features gpu,jpegxl` | `cargo install --path crates/flashraw-cli [--features gpu,jpegxl]` or `cargo build --release -p flashraw-cli` (binary `target/release/flashraw`) |
| Python | `pip install flashraw` (NumPy; abi3 wheels for CPython 3.9+) | `cd crates/flashraw-python && maturin develop --release` |
| JavaScript | `npm install flashraw` | `cd npm && npm run build` (needs the WASM toolchain, [docs/wasm.md](docs/wasm.md)) |
| C / C++ | the release archive (planned) | `cargo build --release -p flashraw-capi`, or CMake: `find_package(flashraw)` after `cmake --install` |

Cargo features of `flashraw`: `rayon`, `simd`, `mmap`, `icc` (all default), `gpu`, `jpegxl`, `serde`.

## Rust

```rust
use flashraw::{ColorSpace, DecodeOptions, Demosaic, FlashRaw, Limits, OutputFormat, Rect};

fn main() -> flashraw::Result<()> {
    let path = std::env::args().nth(1).unwrap();

    // Metadata only: no pixels are decoded.
    let bytes = std::fs::read(&path)?;
    let meta = flashraw::read_metadata(&bytes)?;
    println!("{} {}x{} ISO {:?}", meta.camera(), meta.width, meta.height, meta.exposure.iso);

    // Open untrusted input with explicit limits (see "Untrusted files").
    let limits = Limits::default().with_max_samples(200_000_000).with_max_dimension(20_000);
    let raw = FlashRaw::open_with(bytes.as_slice(), limits)?; // also: open(Vec<u8>), open_path(path), from_reader(r)

    // Embedded JPEG preview: zero-copy bytes, no sensor decode.
    if let Some(p) = raw.preview() {
        std::fs::write("preview.jpg", raw.preview_bytes(p)?)?;
    }

    // Display-ready 8-bit RGBA in Display P3. Orientation is applied for Rgba8 and Rgb16 only.
    let frame = raw.decode(DecodeOptions {
        output: OutputFormat::Rgba8,
        color_space: ColorSpace::DisplayP3,
        limits, // the limits also apply to the decode
        ..Default::default()
    })?;
    let rgba: &[u8] = frame.as_u8().unwrap(); // width * height * 4
    println!("{}x{} {} bytes, ICC profile {} bytes", frame.width(), frame.height(), rgba.len(),
        frame.icc_profile().map_or(0, |p| p.len()));
    for w in frame.warnings() {
        eprintln!("warning: {} ({} samples zero-filled)", w.kind(), w.samples_filled);
    }

    // Half-size (2x2 binning, fastest), 16-bit sRGB.
    let half = raw.decode(DecodeOptions { output: OutputFormat::Rgb16, demosaic: Demosaic::HalfSize, ..Default::default() })?;
    let _rgb16: &[u16] = half.as_u16().unwrap();

    // Sensor data (default output RawU16): CFA mosaic plus what is needed to interpret it.
    let sensor = raw.decode(DecodeOptions::default())?;
    println!("{}x{} cfa {:?} black {:?} white {:?} orientation {:?}", sensor.width(), sensor.height(),
        sensor.cfa, sensor.black_level.values, sensor.white_level.values, sensor.orientation);

    // A region of the sensor buffer (sensor coordinates).
    let _part = raw.decode_region(Rect { x: 100, y: 100, width: 256, height: 256 }, DecodeOptions::default())?;

    // Linear float RGB (camera RGB to linear sRGB primaries, sensor orientation).
    let lin = raw.decode(DecodeOptions { output: OutputFormat::LinearRgbF32, ..Default::default() })?;
    let _f32: &[f32] = lin.as_f32().unwrap();
    Ok(())
}
```

Other outputs: `RawF32`, `NormalizedF32`, `LinearRgb16`, `LinearRgbF16` (`as_f16_bits()`). Other entry points:
`decode_tile(index, opts)`, `units()`, `thumbnail()`, `decode_preview_rgb(preview)`, `supports(bytes)`,
`capabilities()`. Arbitrary ICC output profile (feature `icc`, default): `DecodeOptions::output_profile`.
Full API: <https://docs.rs/flashraw> (after release) or `cargo doc --open -p flashraw`.

### Errors and warnings

```rust
match FlashRaw::open(bytes.as_slice()) {
    Ok(raw) => { /* ... */ }
    Err(flashraw::Error::UnsupportedFormat(why)) => eprintln!("not a RAW file: {why}"),
    Err(e) => eprintln!("{}: {e}", e.kind()), // "UnsupportedCompression", "CorruptFile", "LimitExceeded", ...
}
```

`Error` is `#[non_exhaustive]`: always keep a wildcard arm. `e.kind()` is a stable string. `UnsupportedCompression`
means a valid file in a mode that is not implemented (Nikon High Efficiency NEF): metadata and previews still
work, and `DecodeOptions::preview_fallback = true` makes RGB decodes return the embedded full-size preview
instead (marked `frame.source == FrameSource::Preview` and with a `preview-fallback` warning; it is the
camera's rendering, not sensor data).

Truncated or corrupt sensor data: by default (`CorruptionPolicy::Recover`) what can be decoded is decoded,
the rest is zero-filled, and `frame.is_incomplete()` is true with one entry per repair in `frame.warnings()`.
Check them before trusting an image. `CorruptionPolicy::Strict` fails with `Error::CorruptFile`
(`e.truncation()` has the details) instead. Some single-stream formats (for example Canon CR2) cannot be
recovered and fail under both policies.

### Untrusted files

Always set `Limits` for files that users upload. The defaults (1.2 billion samples, 65,535 pixels per side)
allow 400 MP medium format and can mean several gigabytes for one decode. Pass limits to `open_with` and
to `DecodeOptions::limits`. Do not pass a user-controlled value as `threads`. Read the bytes yourself
(`std::fs::read`, `Vec<u8>`) rather than `open_path` for files other processes may modify: `open_path`
memory-maps, and a file truncated while mapped raises `SIGBUS`. See [SECURITY.md](SECURITY.md).

### Threads, SIMD, GPU

* `DecodeOptions::threads`: `None` (default) uses all cores; `Some(1)` decodes on the calling thread;
  `Some(n)` uses a pool of `n` threads (cached per `n`). Decoding is internally parallel; do not wrap it in
  your own thread pool for a single file, but you can decode different files concurrently (`FlashRaw` and
  `RawFrame` are `Send + Sync`).
* `Backend::Auto` (default) picks per file and output; `Cpu` is the scalar reference, `CpuSimd` adds SIMD
  unpack kernels (SSSE3/SSE4.1 on x86-64, NEON on aarch64, SIMD128 in WebAssembly). `Backend::Gpu` needs
  the `gpu` cargo feature and an adapter, else `Error::GpuUnavailable`; check `flashraw::gpu_available()`.
  The GPU result is within rounding of the CPU result, not bit-identical, and output profiles that use
  lookup tables fall back to the CPU. `Auto` never needs configuration; do not select the GPU "to be fast"
  for small images.

## Command line

`flashraw --help` and `flashraw <command> --help` are authoritative; do not invent options.

```sh
flashraw info photo.CR2                 # summary: format, camera, geometry, levels, WB, previews (--json for scripts)
flashraw info --json photo.CR2          # machine-readable
flashraw dump-metadata photo.CR2        # everything, as JSON
flashraw preview photo.CR2 preview.jpg  # largest embedded preview; `thumbnail` for the smallest
flashraw decode photo.CR2 out.jpg --output rgba8             # developed 8-bit sRGB JPEG
flashraw decode photo.CR2 out.tif --output rgb16 --color-space display-p3   # 16-bit TIFF with ICC profile
flashraw decode photo.CR2 half.ppm --output rgb16 --demosaic half           # half resolution
flashraw decode photo.CR2 sensor.raw                         # raw u16 little-endian samples (default output)
flashraw decode photo.CR2 part.raw --region 100,100,256,256  # sensor region x,y,width,height
flashraw decode photo.CR2 lin.raw --output linear-rgb-f16    # half-float linear RGB bits
flashraw verify photo.CR2 --json        # geometry, levels and hashes of the decoded buffer (--strict fails on truncation)
flashraw supported [--json]             # every format and mode with its status
flashraw benchmark photo.CR2 --iterations 10 --threads 8
```

The output file format follows the extension (`.raw`, `.pgm`/`.ppm`, `.tif`/`.tiff`, `.jpg`) or `--format`.
Other `decode` options: `--backend auto|cpu|simd|gpu`, `--threads N`, `--tile N`, `--no-opcodes`,
`--exposure EV`, `--black-point`, `--white-point`, `--strict`, `--preview-fallback`, `--output-profile FILE.icc`
with `--intent` and `--bpc`, `--profile` (timings), `--json`. `cargo install flashraw-cli` has no `gpu` or
`jpegxl` feature unless you add `--features gpu,jpegxl`.

**Is my camera or format supported?** `flashraw supported` lists format, mode, compression and status:
`verified` (pinned hashes on sample files), `partial` (part of the mode), `experimental` (no sample files),
`unsupported` (rejected with an error), `supported` (a feature such as DNG opcode lists). The definitive test
is the file: `flashraw info FILE` shows the format, geometry, and whether the camera database knows it;
`flashraw verify FILE` decodes it. Per-vendor details: [docs/formats.md](docs/formats.md).

## Python

```python
import flashraw

with flashraw.open("photo.CR2", max_samples=200_000_000, max_dimension=20_000) as raw:  # path, bytes, file object
    m = raw.metadata                       # no pixels decoded
    print(m.camera.name, m.width, m.height, m.exposure.iso)
    jpeg = raw.preview_bytes()             # embedded JPEG bytes or None
    f = raw.decode(output="rgba8", color_space="display-p3")   # NumPy uint8 (h, w, 4)
    pixels = f.array
    print(f.incomplete, [w for w in f.warnings], f.source, len(f.icc_profile or b""))
    half = raw.decode("rgb16", demosaic="half-size")          # uint16 (h/2, w/2, 3)
    sensor = raw.decode()                   # uint16 (h, w) mosaic, with sensor.cfa, .black_level, .white_level
    part = raw.decode(region=(100, 100, 256, 256))

img = flashraw.imread("photo.CR2")          # one-liner: RGBA8 array
try:
    flashraw.open(b"not a raw file")
except flashraw.FlashRawError as e:         # UnsupportedFormatError, UnsupportedCompressionError, CorruptFileError, ...
    print(type(e).__name__, e)
```

Outputs: `raw-u16` (default), `raw-f32`, `normalized`, `linear-rgb`, `linear-rgb-f16`, `linear-rgb16`, `rgba8`,
`rgb16`. Other keywords: `backend`, `threads`, `crop`, `apply_opcodes`, `corruption_policy`, `exposure`,
`black_point`, `white_point`, `preview_fallback`, `output_profile`, `intent`. Decoding releases the GIL:
decode files from a `ThreadPoolExecutor` with `threads=1` each. `Frame.to_pil()` needs `flashraw[pil]`.
Reference: [docs/python.md](docs/python.md).

## JavaScript / TypeScript

```js
import { readFile } from 'node:fs/promises';
import { FlashRaw } from 'flashraw';   // Node.js >= 18; browsers: pass a File, Blob, ArrayBuffer or Uint8Array

const raw = await FlashRaw.open(await readFile('photo.CR2'), {
  limits: { maxSamples: 200_000_000, maxDimension: 20_000 },
});
console.log(raw.metadata.camera.name, raw.metadata.width, raw.metadata.height, raw.metadata.exposure.iso);
const preview = await raw.preview();                 // { data: Uint8Array, mimeType, width, height, orientation } | null
const img = await raw.decode({ output: 'rgba8', colorSpace: 'display-p3' });
// img.data is a Uint8ClampedArray: new ImageData(img.data, img.width, img.height)
const half = await raw.decode({ output: 'rgb16', demosaic: 'half-size' });
const sensor = await raw.decode();                   // Uint16Array mosaic; sensor.cfa, sensor.blackLevel, sensor.whiteLevel
const part = await raw.decode({ region: { x: 100, y: 100, width: 256, height: 256 } });
raw.close();                                          // frees WASM memory
```

Errors are `FlashRawError` subclasses with a `kind` (`err.kind === 'UnsupportedFormat'`). Warnings and
`incomplete` are on the frame. Do not use `FlashRaw.open` in a browser main thread for many large files:
use the worker pool (`import { ... } from 'flashraw/worker'`).

* **Threads in browsers need cross-origin isolation.** Serve the page with
  `Cross-Origin-Opener-Policy: same-origin` and `Cross-Origin-Embedder-Policy: require-corp` (or
  `credentialless`); without them the same API runs single-threaded (check
  `(await getCapabilities()).threads`). Node.js is always threaded.
* WASM variants (chosen at run time, one is downloaded): SIMD (default), scalar fallback for engines without
  SIMD, threaded, WebGPU (`init({ gpu: true })`, WebGPU only, no WebGL) and two JPEG XL variants, loaded only
  when a JPEG XL DNG is decoded. See [docs/wasm.md](docs/wasm.md) and [npm/README.md](npm/README.md).

## C and C++

```c
#include <flashraw.h>
#include <stdint.h>
#include <stdio.h>

int main(int argc, char **argv) {
    (void)argc;
    flashraw_open_options oo;                       /* limits for untrusted files */
    flashraw_open_options_init(&oo);
    oo.max_samples = 200u * 1000 * 1000;
    oo.max_dimension = 20000;

    flashraw_file *f = NULL;
    if (flashraw_open_path(argv[1], &oo, &f) != FLASHRAW_OK) {
        fprintf(stderr, "[%s] %s\n", flashraw_last_error_kind(), flashraw_last_error_message());
        return 1;
    }
    size_t len = 0;                                 /* embedded preview, no decode */
    const uint8_t *jpeg = flashraw_file_preview_count(f) ? flashraw_file_preview_bytes(f, 0, &len) : NULL;
    (void)jpeg;

    flashraw_decode_options o;
    flashraw_decode_options_init(&o);
    o.output = FLASHRAW_OUTPUT_RGBA8;
    flashraw_frame *fr = NULL;
    if (flashraw_decode(f, &o, &fr) == FLASHRAW_OK) {
        const uint8_t *rgba = flashraw_frame_data(fr);  /* width x height x 4 bytes */
        printf("%ux%u first byte %u\n", flashraw_frame_width(fr), flashraw_frame_height(fr), rgba[0]);
        flashraw_frame_free(fr);
    }
    flashraw_file_free(f);
    return 0;
}
```

C++ (`flashraw.hpp`, header-only, C++17, throws `flashraw::error`):

```cpp
#include <flashraw.hpp>
flashraw::file raw = flashraw::file::open("photo.CR2");
flashraw::decode_options o;
o.output = flashraw::output_format::linear_rgb_f32;
flashraw::frame img = raw.decode(o);
flashraw::span<const float> rgb = img.f32();       // no copy
```

Link with `-lflashraw_capi` (or `find_package(flashraw)` and `flashraw::flashraw`). Option structs are
size-versioned: always call the `*_init` function. Handles are freed with the matching `*_free`. A file
handle may be shared between threads; a frame must not be freed while in use. Reference:
[docs/c-api.md](docs/c-api.md).

## Pitfalls

* **Raw samples are not 8-bit.** `raw-u16` holds 10 to 16-bit values (`bits_per_sample`), not scaled to 16;
  for floating-point DNGs use `raw-f32` (unscaled; the Rust `RawU16` output may hold `f32` samples for them, the Python docs say `raw-u16` is scaled to integers). Use the black level and white level from the frame; black
  level may be a per-channel pattern with row and column deltas.
* **Raw and linear outputs keep the sensor buffer**: the full buffer including optical-black borders, not
  cropped (`activeArea`/`crop` tell you the image area), unrotated. Only `rgba8` and `rgb16` apply the EXIF
  orientation (and crop by default); `linear-rgb*` outputs are cropped but keep the sensor orientation: read
  `orientation` from the frame.
* **`rgba8` and `rgb16` are display-referred** (colour space transfer curve applied, default sRGB); the
  `linear-*` outputs are linear light. `linear-rgb-f16` saturates beyond 65504.
* **Memory.** A decode needs the file plus the output, and RGB outputs several times the output size at their
  peak: on a 40 MP file (measured) `raw-u16` peaks at about 160 MB, `rgba8` at about 540 MB and linear float
  at about 770 MB. 100 MP+ files need gigabytes; use `half-size`, `region`, tiles or previews.
* **Parallelism is already inside a decode.** Do not oversubscribe: for many files in parallel pass
  `threads=1` (Python) or `Some(1)` (Rust) per decode.
* **`open_path`/Python paths memory-map the file**: do not truncate or overwrite it while it is open.
* **GPU** is optional and falls back to the CPU for output profiles with lookup tables; results differ from
  the CPU by rounding. In WASM the WebGPU build is single-threaded.
* **WASM without cross-origin isolation is single-threaded** (slower on big files, same results).
* **ICC**: `color_space` embeds the matching profile in `frame.icc_profile`; `output_profile` converts to any
  RGB ICC profile (needs the `icc` feature, on by default; not in the smallest builds).
* **Status words**: `unsupported` modes fail with a clear error; `experimental` modes lack sample files.
* **Names differ between bindings** (`simd`/`cpu-simd`, `half`/`half-size`): [docs/api-names.md](docs/api-names.md).

## Do not

* Do not reimplement demosaicing, white balance or colour matrices from the raw buffer unless you need a
  different algorithm; the metadata gives you everything if you do.
* Do not shell out to other RAW converters as a fallback; use `preview_fallback` for undecodable files.
* Do not invent options or output names: `flashraw --help`, the type definitions and
  [docs/api-names.md](docs/api-names.md) are the source of truth.
* Do not decode user uploads without `Limits`, and do not take `threads` from user input.
* Do not assume every camera works: check `flashraw supported` and handle `UnsupportedCompression`.
* Do not ignore `warnings`/`incomplete`: a recovered frame has zero-filled areas.

## Documentation

[README](README.md) · [packages](docs/packages.md) · [formats](docs/formats.md) ·
[colour management](docs/color-management.md) · [Python](docs/python.md) · [npm](npm/README.md) ·
[WASM](docs/wasm.md) · [C API](docs/c-api.md) · [GPU](docs/gpu.md) · [security](SECURITY.md) ·
[benchmarks](docs/benchmarks.md)

## What was verified

The Rust, Python, JavaScript and C/C++ snippets above (the same calls, with extra prints) and the CLI
commands were run on a Canon EOS 40D CR2 from the CC0 corpus on Linux x86-64: they compile and return the
documented types and sizes, errors and limits fire as described. The Python and C runs used libraries
built earlier from this tree (the Python extension without a wheel build); the JavaScript run used the built
`npm/dist`. Not run: GPU (no adapter on the test machine), browsers, Windows, macOS, the `pip`, `npm` and
`cargo` install lines (checked only after publishing, for pip and npm), cross-origin-isolation behaviour in a browser.

## Working on this repository

* Layout: `crates/flashraw` (public API), `-core`, `-codecs`, `-formats` (decoders), `-camera-db` (data in
  `data/cameras.json`), `-gpu` (WGSL in `shaders/`), `-cli`, `-capi`, `-wasm`, `-python`, `-libraw-compat`;
  `npm/`, `demo/`, `fuzz/`, `tools/`, `docs/`. The decoder contract is [docs/contributing.md](docs/contributing.md);
  how to contribute is [CONTRIBUTING.md](CONTRIBUTING.md).
* Build and test: `cargo build --release`, `cargo test`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo fmt --all --check`. Corpus tests (`cargo test --release -p flashraw --test regression --test malformed`)
  need the CC0 corpus in `corpus/` (`python3 tools/corpus/fetch.py`) and skip or fail loudly without it.
* Local CI: `tools/ci/local.sh` (`--quick`, `--full`, `--list`); macOS via `tools/ci/mac.sh`. The GitHub
  workflows are manual-only on purpose: do not add workflows that run automatically, and do not describe CI
  as automatic. Run the local CI before reporting work as done.
* Camera database policy: every value in `crates/flashraw-camera-db/data/cameras.json` must be backed by sample
  files (CC0 or the maintainers' own); never copy numbers from other projects. Validate with
  `flashraw camera-db validate`; tools in `tools/camera_db/`.
* Other RAW libraries may be named only in README.md, docs/benchmarks.md, tools/bench, benches/results,
  docs/charts, `crates/flashraw-libraw-compat` and docs/libraw-compat.md. Do not add comparisons elsewhere.
* Claims in docs must be measured or checkable: check counts and platforms against `flashraw supported`,
  `docs/formats.md` and `docs/testing.md`.
* Commits: small, focused, imperative subject; do not add generated or unrelated files. Do not push; the
  maintainer pushes. Do not stage with `git add -A` when other people work in the same checkout.
