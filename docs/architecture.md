# FlashRaw architecture

FlashRaw is organised around one idea: **parse cheaply, decode in parallel straight
into the final buffer, never copy what you can borrow.**

```
RAW bytes (borrowed slice / Vec / Arc / mmap / ArrayBuffer)
   │
   ▼
format detection (magic signatures, then one TIFF parse dispatched by EXIF Make)
   │
   ▼
container parser ─► RawMetadata (EXIF, maker notes, previews)        ← available immediately
   │               FrameInfo  (geometry, CFA, levels, WB, matrices)  ← available immediately
   ▼
codec(s) over independent units (tiles / strips / slices / blocks / rows)
   │     CPU scalar ─ CPU SIMD ─ multithreaded ─ GPU (packed payloads)
   ▼
RawImage (u16 or f32 sensor samples, full buffer incl. masked borders)
   │
   ▼
RawFrame  (+ optional fused post-processing: normalise / demosaic / WB / colour / sRGB)
```

## Crates

| crate | role | depends on |
|---|---|---|
| `flashraw-core` | errors, byte/bit readers, Huffman, TIFF/BMFF, EXIF, value types, `RawDecoder` contract, parallel helpers, profiling | — |
| `flashraw-codecs` | compression codecs (lossless JPEG, packed + SIMD, deflate/float, DCT-12, lossy JPEG, JPEG XL, vendor codecs) | core |
| `flashraw-camera-db` | camera database compiled from `camera-db/cameras.json` at build time | — |
| `flashraw-formats` | container parsers + per-vendor decoders | core, codecs, camera-db |
| `flashraw` | public API, backend selection, post-processing | all of the above |
| `flashraw-gpu` | optional wgpu/WebGPU compute backend (WGSL in `shaders/`) | core |
| `flashraw-cli` | `flashraw` binary | flashraw |
| `flashraw-wasm` | wasm-bindgen bindings for the npm package | flashraw |
| `flashraw-python` | PyO3 bindings for the `flashraw` Python package (built with maturin, not a default workspace member) | flashraw |
| `flashraw-capi` | stable C ABI (`flashraw.h`) and header-only C++ wrapper ([c-api.md](c-api.md)) | flashraw |
| `flashraw-libraw-compat` | compatibility C API for existing clients ([libraw-compat.md](libraw-compat.md)) | flashraw |

The split keeps codecs independent of containers (each codec is fuzzed and
benchmarked alone) and keeps the camera database out of the codec crates (WASM
builds pay for it once). We deliberately avoided per-vendor crates: they would
multiply compile units without improving isolation, because vendor modules share
nearly all infrastructure.

## Key decisions

**Sensor-domain output.** `RawFrame` holds samples exactly as decoded (u16, or f32
for floating-point DNG), the full buffer including masked borders, plus everything
needed to interpret them. Development (demosaic, colour) is optional and lives
outside the decoders. This keeps decoding exact and lets tests pin full buffers
bit-for-bit (regression hashes).

**One coordinate system.** CFA patterns, black-level patterns, active area, crop and
masked areas are all expressed in full-buffer coordinates anchored at `(0, 0)`.
Formats that anchor elsewhere (DNG anchors at ActiveArea; many maker notes describe the
CFA at the visible origin) are converted at parse time, so consumers never deal with
offsets.

**Borrowing and zero-copy.** Decoders borrow `&'a [u8]` from the input. `FlashRaw`
can own its input (`Vec`, `Arc<[u8]>`, mmap) and still hand the decoder a borrow via
a narrowly scoped, documented self-referential pattern (only heap-stable owners are
accepted; the decoder is dropped first). Previews are returned as slices of the input.

**Allocation discipline.** Output buffers come from `alloc_zeroed` (calloc semantics:
the OS provides zero pages lazily, so a 60 MP buffer costs no memset) after a limit
check. Codecs decode into row sinks or directly into tiles of the final buffer; no
codec allocates a full-size intermediate. The fused develop pass goes from u16
sensor data to the final encoding (RGBA8, 16-bit sRGB, linear 16-bit, half or full floats;
display encodings oriented) in one sweep with a 4-row ring of normalised rows. DNG opcode
lists 2 and 3 stream through the same pass: rows are normalised and list 2 applied as the
demosaic reads them, and list 3 runs on each task's band of camera RGB before white
balance and colour conversion (lists with bad-pixel repairs, which read neighbours, fall
back to whole-frame buffers). Staggered lattices (Fujifilm SuperCCD, DNG CFALayout 2-5) and
non-square photosites are the exception: they develop to linear RGB in the stored grid, are
resampled to square pixels (`stagger.rs`) and then encoded, so their RGB outputs differ in
size from the crop.

**Parallelism that respects the format.** `flashraw_core::par` offers row-chunk,
index and tile-grid parallelism (rayon on native, sequential elsewhere).
`GridWriter` lets threads write disjoint tiles of one buffer concurrently: tiles are
claimed through atomic flags (each at most once) and every row slice is clipped to
its tile, which is what makes the `unsafe` inside it sound. Formats declare
`RandomAccess` honestly (`Sequential`, `Rows`, `Strips`, `Tiles`, `Blocks`), and
region decoding only touches intersecting units when the format permits it.

**SIMD with references.** Every optimised kernel has a scalar reference (the bit
pump based `unpack_row_scalar`, naive bilinear in tests, etc.). Fast paths are
tested bit-for-bit against references, including on truncated inputs (the fuzzer
found and pinned one such divergence). x86-64 kernels are runtime-dispatched
(SSSE3/SSE4.1); aarch64 uses NEON; WASM uses SIMD128 when compiled with it.

**Bit pumps.** MSB, LSB, MSB16, MSB32 and JPEG (byte-unstuffing) pumps share one
design: a 64-bit cache refilled 8 bytes at a time to ≥56 bits without per-byte
branches, a bounds-checked tail that pads zeros, and overrun accounting
(`past_end()` = bytes *consumed* beyond the input) so truncated streams are detected
precisely without per-symbol checks.

**Huffman.** Canonical tables with a 12-bit LUT whose entries can hold the
fully-decoded difference (code + diff bits ≤ 12), so most lossless-JPEG samples
decode with one lookup and one shift.

**Parallel decoding of one Huffman stream.** Formats that store the image as one
sequential prefix-coded stream (lossless JPEG without restart markers in CR2, sRAW,
single-strip DNG and Kodak DCS; Hasselblad pair coding; Nikon and Pentax predictive
Huffman; Panasonic v8 strips; Sony ARW1) are still decoded on all threads (`flashraw-codecs::par_entropy`): the stream is cut into chunks that are decoded speculatively (one full decode
per chunk; starts in other table phases run only until they join it), Huffman
self-synchronisation locates where each chunk's speculation joins the true path
(typically within a few dozen symbols), and rows are rebuilt from the differences by
a parallel prefix sum. Any case the speculation cannot prove identical — no
synchronisation, an invalid code, truncation — falls back to the sequential decoder,
so results and errors never differ. Lossless JPEG scans with restart markers (common
in phone DNGs) need no speculation: each restart interval is an independent bit stream
with reset predictors and is decoded as its own task. Where samples depend on rows
above (lossless JPEG predictors 2-7, Samsung V0), reconstruction runs as a wavefront
(`par::wavefront`): rows are cut into bands and columns into blocks, and a block starts
as soon as its left neighbour and the block above it are done.

**Camera knowledge as data.** `camera-db/cameras.json` — one database for the whole
library — holds camera entries (aliases, modes, crops, masked areas, per-ISO levels,
CFA, colour matrices, decoder hints, verification records), fallback colour tables,
model-alias groups and size-based geometry rules. `build.rs` compiles it into static
arrays plus a sorted key index — no parsing at runtime.
Decoders use file metadata first and the database only to fill gaps, so unknown
cameras still decode when the file is self-describing (DNG always is).

**Safety.** All parsing is bounds-checked; offsets use checked arithmetic; IFD
traversal tracks visited offsets and depth; allocations are limit-checked before they
happen; entropy decoders stop on overrun. `unsafe` is confined to: zeroed allocation,
`GridWriter`, SIMD intrinsics, the input self-reference, the WASM clock hook, the Apple
worker quality-of-service call, the mmap call and the pointer handling at the C ABI boundary (`flashraw-capi`,
`flashraw-libraw-compat`) — each with a `SAFETY:` justification and tests. Nine fuzz targets plus a
malformed-file suite run in CI.

**Profiling built in.** Decoders record stage timings (`Stage::ContainerParse`,
`EntropyDecode`, `Unpack`, `Calibration`, `Postprocess`, GPU stages...) into a
`Profiler`; `DecodeStatistics` reports them with the backend actually used. On WASM a
`performance.now()` clock is installed through `profile::set_clock`.

## Progressive / interactive use

`FlashRaw::open` parses headers only (typically well under 1 ms), so metadata and
embedded previews (`preview()`, `thumbnail()`, `preview_bytes()` — zero-copy) are
available before any pixel decoding. `decode_region`/`decode_tile` let viewers decode
what is visible first on tiled formats.

## Remote / range-based reading

Parsers address the payload through `DecodeUnit { rect, offset, length }`; the units
of a tiled file describe exactly which byte ranges a region needs. Today the input
must be fully present; the unit model is what allows a future range-request source
(header fetch → unit selection → HTTP Range reads) without changing decoders.

## Backends

`Backend::Auto` uses the CPU (SIMD + threads) unless the GPU backend is compiled in
and its heuristics predict a win (see [gpu.md](gpu.md)); `Cpu` forces scalar
reference kernels (useful for verification), `CpuSimd` explicit SIMD, `Gpu` the wgpu
backend. `DecodeStatistics::backend` reports what actually ran.
