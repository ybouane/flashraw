# Names across the bindings

The same options carry different spellings in Rust, the command line, Python, JavaScript and C.
Parsers accept the aliases listed; the canonical name is the first one. Each row was checked against
`crates/flashraw/src/options.rs` (`parse` / `name`), the CLI `--help`, `python/flashraw/__init__.py`,
`npm/src/index.d.ts` and `crates/flashraw-capi/include/flashraw.h`.

## Values

| Concept | Rust | CLI | Python | JavaScript | C |
|---|---|---|---|---|---|
| Backend: automatic | `Backend::Auto` | `auto` | `"auto"` | `'auto'` | `FLASHRAW_BACKEND_AUTO` |
| Backend: scalar CPU | `Backend::Cpu` (`"cpu"`, alias `scalar`) | `cpu` | `"cpu"` | `'cpu'` (alias `'scalar'`) | `FLASHRAW_BACKEND_CPU` |
| Backend: CPU with SIMD unpack kernels | `Backend::CpuSimd` (`"simd"`, alias `cpu-simd`) | `simd` | `"simd"` | `'cpu-simd'` (alias `'simd'`) | `FLASHRAW_BACKEND_CPU_SIMD` |
| Backend: GPU | `Backend::Gpu` (`"gpu"`, alias `webgpu`) | `gpu` | `"gpu"` | `'gpu'` (alias `'webgpu'`) | `FLASHRAW_BACKEND_GPU` |
| Demosaic: 2x2 binning, half resolution | `Demosaic::HalfSize` (`"half-size"`, aliases `half`, `halfsize`) | `half` (also `half-size`) | `"half-size"` | `'half-size'` | `FLASHRAW_DEMOSAIC_HALF_SIZE` |
| Demosaic: bilinear | `Demosaic::Bilinear` | `bilinear` | `"bilinear"` | `'bilinear'` | `FLASHRAW_DEMOSAIC_BILINEAR` |
| Output: sensor samples as stored | `OutputFormat::RawU16` (`"raw-u16"`, aliases `raw`, `u16`) | `raw-u16` | `"raw-u16"` | `'raw-u16'` | `FLASHRAW_OUTPUT_RAW_U16` |
| Output: sensor samples as f32 | `RawF32` (`"raw-f32"`) | `raw-f32` | `"raw-f32"` | `'raw-f32'` | `FLASHRAW_OUTPUT_RAW_F32` |
| Output: black-subtracted, white-normalised f32 | `NormalizedF32` (`"normalized"`) | `normalized` | `"normalized"` | `'normalized'` | `FLASHRAW_OUTPUT_NORMALIZED_F32` |
| Output: linear RGB f32 | `LinearRgbF32` (`"linear-rgb"`, aliases `linear-rgb-f32`, `rgb-f32`) | `linear-rgb` | `"linear-rgb"` | `'linear-rgb'` | `FLASHRAW_OUTPUT_LINEAR_RGB_F32` |
| Output: linear RGB 16-bit integers | `LinearRgb16` (`"linear-rgb16"`) | `linear-rgb16` | `"linear-rgb16"` | `'linear-rgb16'` | `FLASHRAW_OUTPUT_LINEAR_RGB16` |
| Output: linear RGB half floats | `LinearRgbF16` (`"linear-rgb-f16"`) | `linear-rgb-f16` | `"linear-rgb-f16"` | `'linear-rgb-f16'`, or `'linear-rgb'` with `sampleType: 'f16'` | `FLASHRAW_OUTPUT_LINEAR_RGB_F16` |
| Output: display sRGB 8-bit RGBA | `Rgba8` (`"rgba8"`, aliases `rgba`, `imagedata`) | `rgba8` | `"rgba8"` | `'rgba8'` | `FLASHRAW_OUTPUT_RGBA8` |
| Output: display sRGB 16-bit RGB | `Rgb16` (`"rgb16"`, aliases `srgb16`, `rgb-u16`) | `rgb16` | `"rgb16"` | `'rgb16'` | `FLASHRAW_OUTPUT_RGB16` |
| Output: GPU-resident | `GpuRawU16`, `GpuNormalizedF32`, `GpuLinearRgbF32`, `GpuRgba8` | - | - | `'gpu-raw-u16'`, `'gpu-normalized'`, `'gpu-linear-rgb'`, `'gpu-rgba8'` | - |
| Corruption policy | `CorruptionPolicy::Recover` / `Strict` | `--strict` (recover by default) | `corruption_policy="recover"` / `"strict"` | `corruptionPolicy: 'recover' \| 'strict'` | field of `flashraw_decode_options` |

`Backend::CpuSimd` differs from `Backend::Cpu` only in the unpack kernels (SSSE3/SSE4.1 on x86-64, NEON on
aarch64, SIMD128 on WebAssembly); the rest of the develop pipeline is the same compiled code.

## Options

| Concept | Rust (`DecodeOptions`) | CLI | Python (`decode`) | JavaScript (`decode`) |
|---|---|---|---|---|
| Output format | `output` | `--output` | `output` (first argument) | `output` |
| Backend | `backend` | `--backend` | `backend` | `backend` (+ `fallback: 'cpu' \| 'error'`) |
| Threads | `threads: Option<usize>` (`None` = all cores) | `--threads` | `threads` (`None` = all cores) | `init({ threads })`, not per decode |
| Region | `region: Option<Rect>` or `decode_region` | `--region x,y,w,h` | `region=` or `decode_region` | `region` |
| Tile | `decode_tile(index, opts)` | `--tile N` | `decode_tile(index)` | `decodeTile(index)` |
| Crop RGB outputs | `crop: bool` | (always cropped) | `crop=True \| "active"` (`"active"` is Python/JS only) | `crop: true \| 'active'` |
| DNG opcodes | `apply_opcodes` | `--no-opcodes` | `apply_opcodes` | `applyOpcodes` |
| Preview fallback | `preview_fallback` | `--preview-fallback` | `preview_fallback` | `previewFallback` |
| Colour space | `color_space: ColorSpace` | `--color-space` | `color_space` | `colorSpace` |
| ICC profile | `output_profile`, `RenderingIntent`, black point compensation | `--output-profile`, `--intent`, `--bpc` | `output_profile`, `intent`, `black_point_compensation` | `outputProfile`, `intent`, `blackPointCompensation` |
| Exposure, black and white point | `exposure`, `black_point`, `white_point` | `--exposure`, `--black-point`, `--white-point` | `exposure`, `black_point`, `white_point` | `exposure`, `blackPoint`, `whitePoint` |
| Limits | `Limits` (`max_samples`, `max_dimension`) | (defaults) | `open(..., max_samples=, max_dimension=)` | `open(input, { limits: { maxSamples, maxDimension } })` |

Colour space names: `srgb`, `display-p3`, `adobe-rgb`, `prophoto`, `rec2020` in every binding (JavaScript also
accepts aliases such as `p3`, `romm`). Rendering intents: `perceptual`, `relative`, `saturation`, `absolute`.

## Words

| Thing | Rust | Python | JavaScript | C |
|---|---|---|---|---|
| An opened file | `FlashRaw` | `RawFile` (`flashraw.open`) | `FlashRaw` | `flashraw_file` |
| A decoded image | `RawFrame` | `Frame` | `DecodedFrame` (`GpuFrame` for GPU outputs) | `flashraw_frame` |
| Camera name | `metadata().camera()` returns a string | `metadata.camera.name` | `metadata.camera.name` (an object with `make`, `model`, `name`) | metadata accessors |
