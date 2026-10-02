# FlashRaw on WebAssembly

WebAssembly is a first-class FlashRaw target: the same Rust decoders run in browsers, Web
Workers, Node.js and Deno through the `flashraw` npm package (`npm/`). This document covers the
architecture, memory model, builds, sizes and performance. User-facing API docs:
[`npm/README.md`](../npm/README.md).

## Architecture

```
 JS API (npm/src)                                   Rust (crates/flashraw-wasm)
 ───────────────────────────────────────────────    ─────────────────────────────────────────
 browser.js / node.js   platform: load .wasm,       lib.rs   wasm-bindgen surface (wasm32 only)
                        read paths/Blobs              InputBuffer → open() → RawFile → Frame
 core.js                FlashRaw, DecodedFrame,     api.rs   option parsing, decode/preview,
                        init(), errors, GPU policy             detection (target-independent,
 worker*.js             worker pool + protocol                 unit-tested natively)
 errors.js              FlashRawError subclasses    json.rs  metadata / frame JSON (camelCase)
 flashraw_bindgen.js    generated glue (1 file      ──────────────────────────────────────────
                        serves both binaries)        flashraw (default-features off + simd)
```

- The wasm-bindgen surface is deliberately small and pointer-shaped. Everything ergonomic
  (classes, typed arrays, option defaults, fallback policy, worker protocol) lives in
  hand-written JS, so the ABI rarely changes and new options do not need new exports.
- Options travel as one JSON string per call; metadata/frame descriptions come back as one JSON
  string, parsed once in JS (one boundary crossing instead of one per field).
- `flashraw-wasm` depends on `flashraw` with `default-features = false, features = ["simd"]`:
  no rayon, no mmap. `#[wasm_bindgen(start)]` installs a `performance.now()` clock via
  `flashraw_core::profile::set_clock` (`Instant` is unavailable on `wasm32-unknown-unknown`) and a
  panic hook that records the panic message for the JS error.
- Default limits on wasm32 are tighter than native (400 M samples instead of 1.2 G): the address
  space is 4 GiB and an allocation failure in WASM is an abort, not an error.
- Errors are JS `Error`s with a `kind` (= `flashraw::Error::kind()`), mapped to subclasses
  (`CorruptFileError`, …). A WASM trap (Rust panic/OOM) becomes `InternalError` with the panic
  message, and the affected handle is marked unusable.

## Memory model and copies at the boundary

| Data | Copies | Lifetime |
|---|---|---|
| Input bytes (`Uint8Array`/`ArrayBuffer`/`Buffer`) | **1** (into an `InputBuffer` allocated in WASM memory with `try_reserve`; OOM → `OutOfMemoryError`) | owned by the `RawFile` until `close()` |
| Input `Blob`/`File` (browser), file path (Node) | **1**, streamed chunk by chunk straight into WASM memory (Node: 8 MiB staging buffer) — no full-size JS copy | same |
| Metadata, frame info | JSON string (small) | plain JS objects, cached per handle |
| Embedded preview (default) | 1 (`slice()` of the input region) | standalone `Uint8Array` |
| Embedded preview (`zeroCopy: true`) | **0**: view into the input buffer | until `close()`; the handle is pinned (not GC-able) meanwhile |
| Decoded pixels (default) | 1 (`slice()` out of WASM memory), WASM buffer freed immediately | standalone, transferable typed array |
| Decoded pixels (`zeroCopy: true`) | **0**: `frame.data` is a view | until `frame.free()` / `using`; pinned until then |
| Worker pool results | 1 inside the worker, then **transferred** (no copy) | standalone |

Rules that make the zero-copy paths safe:

- Allocations never move, but `memory.grow` detaches every `ArrayBuffer` view. `frame.data` and
  zero-copy preview `data` are getters that re-derive the view when `memory.buffer` changed, so
  re-reading the property after other FlashRaw calls is always valid.
- A typed-array view does not keep its owner alive. Zero-copy frames and pinned handles are held
  in a strong set until explicitly freed, so the wasm-bindgen `FinalizationRegistry` can never
  free memory a live view points into. Everything else (handles, copied frames' WASM side) is
  freed deterministically by `close()`/`free()`/`Symbol.dispose`, with the FinalizationRegistry
  as a safety net (tested: dropped handles are reclaimed after GC).
- WASM memory never shrinks, but freed blocks are reused: repeated open/decode/close cycles keep
  `memory.buffer.byteLength` constant after warm-up (asserted in `tests/memory.test.mjs`).
  Peak memory ≈ input + decoded buffer (+ develop intermediates for RGB outputs: f32 RGB).

## Builds

`npm run build` (`npm/scripts/build.mjs`):

1. `cargo build -p flashraw-wasm --target wasm32-unknown-unknown --profile wasm-release` twice:
   with `RUSTFLAGS=-C target-feature=+simd128` (→ `flashraw_simd.wasm`) and without (→
   `flashraw.wasm`), each in its own target dir (`target/wasm-simd`, `target/wasm-fallback`).
   `[profile.wasm-release]`: opt-level 3, fat LTO, 1 codegen unit, `panic = "abort"`, stripped.
2. `wasm-bindgen --target web --omit-default-module-path` with the CLI version pinned to
   `Cargo.lock` (the script installs the matching `wasm-bindgen-cli` into `npm/.tools/` if needed).
   The glue of both variants must be byte-identical (asserted), so one `flashraw_bindgen.js`
   serves both binaries.
3. `wasm-opt -O3` (binaryen from npm devDependencies).
4. Copies the hand-written JS/`.d.ts` into `dist/` and prints the size report.

`--features gpu` forwards to `flashraw/gpu` (see GPU below); `--no-opt` skips wasm-opt.

Runtime selection: `WebAssembly.validate()` of a 31-byte SIMD probe module (same probe as
wasm-feature-detect) picks `flashraw_simd.wasm`, otherwise `flashraw.wasm`; if the SIMD binary
fails to compile, the loader retries with the fallback. Override with `init({ simd: false })`
or `FLASHRAW_WASM_SIMD=0` (Node). Browsers use `fetch` + `instantiateStreaming` (compile while
downloading); Node reads the file with `fs`.

### Threads

Not in the WASM build. A wasm-threads build needs nightly Rust + `-Z build-std` (atomics),
`wasm-bindgen-rayon`, and — in browsers — cross-origin isolation (COOP `same-origin` + COEP
`require-corp`) for `SharedArrayBuffer`. Only stable Rust is available here and the result would
not work on non-isolated pages, so parallelism is provided at the JS level instead:

- `flashraw/worker` → `createWorkerPool({ size })`: N Web Workers (browser) or `worker_threads`
  (Node), each with its own WASM instance. Decoding off the main thread keeps UIs responsive;
  several files decode concurrently. Files (`File`/`Blob`) are passed by reference and read inside
  the worker; Node paths are opened by the worker; `transfer: true` moves ArrayBuffers.
  Measured: 8 files (fixtures + 4 corpus DNGs, rgba8) sequential 4149 ms → 4 workers 1535 ms
  (2.7×) in Node.
- Works without cross-origin isolation. `getCapabilities().threads` reports
  `sharedArrayBuffer`/`crossOriginIsolated`/`hardwareConcurrency` so a future threaded build can
  be selected at runtime (`scripts/serve.mjs --isolated` serves the required headers).

### GPU

The JS API is ready for the WebGPU backend (`flashraw-gpu`, built elsewhere):

- `decode({ backend: 'gpu' | 'webgpu', fallback: 'cpu' | 'error' })`. Without a usable GPU the
  default falls back to the CPU and reports `frame.backend.fallbackReason`; `fallback: 'error'`
  throws `GpuUnavailableError`. Reasons, checked in order: binary built without the `gpu`
  feature; `navigator.gpu` missing; no adapter (`flashraw::gpu_available()`). A
  `GpuUnavailable` error raised by Rust during decode is handled the same way.
- `frame.backend.kind` (`'cpu'|'gpu'`) and `gpuAdapter`, `timings.stages.gpuUpload/gpuCompute/
  gpuDownload` are already part of the result shape; `getCapabilities().gpu` = `{ compiled,
  webgpu, available, reason }`; `buildInfo().gpu` tells JS whether the binary has the backend.
- Integration points left for the GPU work: build with `npm run build -- --features gpu`
  (currently does not compile because `flashraw-gpu` is still a stub); WebGPU adapter requests are
  async on the web, so `RawFile` will need an async decode export (the JS `decode()` is already
  `async`); GPU-resident outputs (`RawPixelData::Gpu`) are rejected with `NotAvailable` in
  `api.rs::into_decoded` until a download/`GPUBuffer` handoff is defined.

## Sizes

`npm run build` output (wasm-bindgen 0.2.129, wasm-opt -O3, binaryen 132):

| Binary | Raw | gzip -9 | brotli -11 |
|---|---|---|---|
| `flashraw_simd.wasm` | 1978 KiB | 678 KiB | 507 KiB |
| `flashraw.wasm` (fallback) | 1946 KiB | 666 KiB | 497 KiB |
| JS (glue + wrappers + worker, unminified) | 67 KiB | — | — |

Sizes include every format decoder (measured after all vendor decoders were merged; the
DNG-only build was 216 KiB brotli). See [benchmarks.md](benchmarks.md#wasm) for the size
breakdown. Only one binary is downloaded per page. Of the SIMD binary, 416 KiB is code and 364 KiB data
(mostly the camera database tables, which compress well). `wasm-opt -Oz` instead of `-O3`
saves < 2 %. `npm pack`: 608 kB tarball, 16 files.

## Init latency

Cold process / fresh page, import + fetch/read + compile + instantiate:

| Environment | SIMD | Fallback |
|---|---|---|
| Node 26 | 17–30 ms | 17–27 ms |
| Chromium 153 (headless, localhost) | 5–20 ms | 4–17 ms |

V8 compiles with Liftoff first and tiers up hot functions with TurboFan in the background, so
the first decode of a session can be slower than steady state (`--no-liftoff` raised init to
~45 ms without improving steady state).

## Benchmarks

`npm run bench -- --browser` (`scripts/bench.mjs`): same files, median of 7–9 runs after 2
warm-ups. WASM = JS wall time of `decode()` including the copy out of WASM memory; native =
`flashraw benchmark --threads 1` / all cores (16). Machine: 16-core x86-64 (AVX-512), **shared
with other heavy jobs during measurement (load average 15–18)**, so treat single cells as ±30 %;
the ratios below were confirmed on repeated quieter runs.

raw-u16 (ms):

| File | MP | Codec | Node SIMD | Node scalar | Chromium SIMD | Chromium scalar | Native 1T | Native 16T |
|---|---|---|---|---|---|---|---|---|
| canon-5d3-mlv.dng | 2.1 | LJPEG | 6.4–8.5 | 6.4 | 6.5 | 6.4 | 5.0 | 1.6 |
| bmpcc-tiles.dng | 2.2 | LJPEG tiles | 12.5 | 10.4 | 11.7 | 10.7 | 7.3 | 7.7 |
| canon-a410-chdk.dng | 3.3 | 10-bit packed | 2.8 | 2.2 | 2.7 | 2.4 | 0.9 | 0.7 |
| Leica M8 | 10.3 | 8-bit linearised | 5.2 | 7.2 | 5.0 | 6.3 | 3.1 | 2.4 |
| Sigma fp 4K 12-bit | 8.4 | 12-bit packed | 2.2–3.9 | 5.6–7.0 | 3.3 | 5.7 | 1.4 | 1.6 |
| iPhone XS | 12.2 | LJPEG | 45.7 | 36.4 | 41.9 | 36.1 | 28.3 | 7.9 |
| Pixel 6 Pro | 12.5 | LJPEG | 44–79 | 44.2 | 44.4 | 44.6 | 34.3 | 9.7 |
| Pentax K-50 | 16.4 | LJPEG | 50.5 | 50.6 | 49.5 | 49.7 | 39.6 | 44.3 |
| canon-5d3-lossy.dng | 10.5 | lossy JPEG, linear RGB | 171 | 160 | 115 | 138 | 60.1 | 16.2 |

rgba8 (decode + demosaic + colour + sRGB, ms):

| File | Node SIMD | Node scalar | Chromium SIMD | Chromium scalar | Native 1T | Native 16T |
|---|---|---|---|---|---|---|
| canon-5d3-mlv.dng (2.1 MP) | 114 | 115 | 113 | 117 | 84 | 16 |
| canon-a410-chdk.dng (3.3 MP) | 173 | 171 | 167 | 167 | 124 | 24 |
| Leica M8 (10.3 MP) | 559 | 635 | 545 | 546 | 448 | 98 |
| Pentax K-50 (16.4 MP) | 912 | 911 | 891 | 889 | 660 | 188 |
| Pixel 6 Pro (12.5 MP) | 707–1009 | 707 | 791 | 692 | 529 | 82 |

Findings:

- **WASM vs native, same thread count**: entropy-decoded formats (LJPEG) run at 1.2–1.6× native
  single-thread time; the full rgba8 pipeline at 1.2–1.4×. Packed/uncompressed formats are
  memory-bound and fast either way (2–3 ms per frame) but 2–3× native, since native uses AVX-512
  unpack kernels. Native's advantage at scale is threads: 4–7× faster again with 16 cores,
  which WASM recovers only across files via the worker pool.
- **SIMD128 vs scalar WASM**: identical output (tested); a clear win only where explicit WASM
  SIMD kernels exist today — the packed-bit unpack (`flashraw-codecs/src/packed.rs`): 12-bit
  Sigma fp 2.2–3.3 ms vs 5.6–7 ms (~2×). Huffman/LJPEG and the develop pipeline have no SIMD128
  kernels yet, so both binaries are within noise there. Porting the develop (demosaic/colour)
  pass to SIMD128 is the largest remaining WASM win (it is 80–90 % of rgba8 time).
- **Node vs Chromium**: same V8 engine, same results within noise.
- **Lossy DNG** spends its time in the JPEG codec (zune-jpeg); WASM is ~2.5× native there.

### Node: WASM instead of a native addon

Node uses the same WASM module as browsers. Rationale:

- **No install-time native build or per-platform prebuilt binaries** (N-API × OS × arch × libc),
  no postinstall scripts, works in serverless, StackBlitz, Deno, Bun and Electron alike; a single
  ~600 kB tarball.
- **Sandboxed**: untrusted RAW files are parsed inside WASM linear memory; a decoder bug cannot
  corrupt the Node process.
- **Cost**: 1.2–1.6× native single-thread time per decode (table above). Applications that need
  full multi-core throughput per image should use the native CLI or the Rust crate; for many
  files, `flashraw/worker` gets most of the parallel throughput back (2.7× with 4 workers).

## Tests

| Command | What |
|---|---|
| `cargo test -p flashraw-wasm` | option parsing, structured errors, DNG round trip (native) |
| `npm test` | Node: metadata/decode/preview vs native CLI hashes (4 bundled CC0 fixtures, 11 corpus DNGs when `$FLASHRAW_CORPUS` is present), all outputs, regions/tiles, input types, zero-copy, GPU fallback, garbage/truncated/corrupted input never traps, limits, memory release + GC reclamation, SIMD vs fallback equality, worker pool |
| `npm run test:types` | `tsc` against the `.d.ts` (bundler and NodeNext resolution) |
| `npm run test:browser` | headless Chromium (Playwright): SIMD binary selected, raw-u16 hash == native, RGBA8 `ImageData` on a canvas, fallback binary agrees, worker pool, GPU fallback, errors |
| `npm run test:bundlers` | `npm pack` → install into scratch apps → Vite 8, esbuild (browser + node), webpack 5 → run in Chromium |

Hash check: `flashraw verify --json` prints `hash_full` = FNV-1a 64 over the LE bytes of the
u16 buffer; `tests/helpers.mjs` computes the same in JS (32-bit halves, no BigInt). Recorded
reference hashes live in `npm/tests/fixtures/expected.json` (regenerate with
`node tests/gen-expected.mjs`); tests prefer the live CLI when `target/release/flashraw` exists.

Chromium needs system libraries; without root, `npm/scripts/fetch-browser-libs.sh` downloads
and extracts them (the test runner picks them up automatically).
