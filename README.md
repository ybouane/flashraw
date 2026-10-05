# FlashRaw

**Fast, safe, cross-platform RAW image decoding in Rust** — with a CLI, WebAssembly
and an npm package for browsers and Node.js, and an optional GPU (wgpu/WebGPU)
backend.

FlashRaw turns camera RAW files into sensor-domain data (`RawFrame`): the decoded
samples plus everything needed to interpret them — CFA layout, black/white levels,
white balance, colour matrices, crops, orientation — and exposes metadata and
embedded previews without decoding the sensor at all.

- **Fast**: 64-bit bit pumps, LUT Huffman decoding, SIMD unpacking (SSE4.1 / NEON /
  WASM SIMD128), tile/strip/slice parallelism, zero-copy parsing, single-pass RGB
  development. See [benchmarks](docs/benchmarks.md).
- **Correct**: every one of ~2,000 CC0 sample files (every make and model on
  raw.pixls.us) is decoded and checked; full-buffer hashes of the decoded data are
  pinned by the regression suite (see [testing](docs/testing.md)).
- **Safe**: bounds-checked parsing, limit-checked allocations, fuzzing, a malformed
  file suite, and minimal, documented `unsafe`.
- **Everywhere**: native library, `flashraw` CLI, WASM + npm (`flashraw`) for
  browsers, Web Workers and Node.js.

## Results at a glance

All 1,980 decodable files of a 2,016-file CC0 corpus (every make and model on
raw.pixls.us) decode; full-buffer hashes are pinned by the regression suite. On 101
representative files (AMD Ryzen 7 9700X, 16 threads):

| | Median throughput | Largest file (154 MP IIQ) |
|---|---:|---:|
| Full decode | **1,960 MP/s** | 53 ms |
| RAW → display RGB (bilinear) | **580 MP/s** | 216 ms |

Single sequential entropy streams (CR2, lossless DNG, Hasselblad, Nikon, Pentax,
Panasonic v8, Sony ARW1) are decoded on all cores by speculation. Details: [docs/benchmarks.md](docs/benchmarks.md).

## Rust

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
truncated or fails to decode is zero-filled with a warning). Other compressed streams
(one entropy-coded stream per image) fail under both policies; when their declared range
runs past the end of the file the error carries the same truncation details.

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

## CLI

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

## Browser and Node.js (npm)

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

## Supported formats

DNG (all compressions incl. lossless JPEG, deflate/float, lossy JPEG, JPEG XL),
CR2, CR3, CRW, NEF, NRW, ARW, SR2, SRF, RAF, RW2, RWL, ORF, ORI, PEF, IIQ, MOS, 3FR,
FFF, MRW, SRW, X3F, KDC, DCR, ERF, MEF and more. Support is tracked per format *and
mode* (bit depth, compression, crop/size modes) with statuses `verified`, `partial`,
`experimental`, `unsupported`: see [docs/formats.md](docs/formats.md) and
`flashraw supported`.

## GPU

Build with `--features gpu`. Packed/uncompressed payloads are unpacked on the GPU;
black subtraction, demosaic, white balance, colour conversion and sRGB encoding run
as WGSL compute shaders shared by native (Vulkan/Metal/DX12) and browser (WebGPU)
builds; results can stay GPU-resident. `Backend::Auto` only picks the GPU when its
measured cost model predicts a win. See [docs/gpu.md](docs/gpu.md).

## Documentation

[Architecture](docs/architecture.md) · [Formats](docs/formats.md) ·
[GPU](docs/gpu.md) · [WASM](docs/wasm.md) · [Testing](docs/testing.md) ·
[Benchmarks](docs/benchmarks.md) · [Camera DB](docs/camera-db.md) ·
[Contributing](docs/contributing.md) · [References](docs/references.md)

## License

Code and data (including `camera-db/`): MIT OR Apache-2.0, at your option. See
[LICENSE-MIT](LICENSE-MIT) and [LICENSE-APACHE](LICENSE-APACHE).
