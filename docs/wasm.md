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
                        GpuFrame, init(), errors,              async GPU decode, detection
                        binary selection                       (target-independent, unit-tested
 threads.js             thread pool (coordinator               natively)
 thread-worker.*.js     + rayon workers)            json.rs  metadata / frame JSON (camelCase)
 worker*.js             worker pool + protocol      ──────────────────────────────────────────
 errors.js              FlashRawError subclasses     flashraw (default-features off + simd;
 flashraw_bindgen*.js   generated glue: one file for           + rayon in the threaded build,
                        simd+fallback, one each for            + gpu in the WebGPU build,
                        threads, gpu and the two                + jpegxl in the JPEG XL builds)
                        JPEG XL variants
```

- The wasm-bindgen surface is deliberately small and pointer-shaped. Everything ergonomic
  (classes, typed arrays, option defaults, fallback policy, worker protocol) lives in
  hand-written JS, so the ABI rarely changes and new options do not need new exports.
- Options travel as one JSON string per call; metadata/frame descriptions come back as one JSON
  string, parsed once in JS (one boundary crossing instead of one per field).
- `flashraw-wasm` depends on `flashraw` with `default-features = false, features = ["simd"]`:
  no rayon, no mmap (the `threads` feature adds rayon, `gpu` the WebGPU backend). `#[wasm_bindgen(start)]` installs a `performance.now()` clock via
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
| Input bytes (`Uint8Array`/`ArrayBuffer`/`Buffer`) | **1** (into an `InputBuffer` allocated in WASM memory with `try_reserve`, not zero-filled since JS overwrites it; OOM → `OutOfMemoryError`) | owned by the `RawFile` until `close()` |
| Input `Blob`/`File` (browser), file path (Node) | **1**, streamed chunk by chunk straight into WASM memory (a Blob whose stream fails is re-read in 8 MiB slices: WebKit rejects streaming fetched Blobs over ~100 MB) — no full-size JS copy (Node paths: read directly into the shared memory of the threaded build, through an 8 MiB staging buffer otherwise) | same |
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

1. `cargo build -p flashraw-wasm --target wasm32-unknown-unknown --profile wasm-release` once
   per variant, each in its own target dir (`target/wasm-<variant>`):

   | Variant | File | Toolchain / flags | Features |
   |---|---|---|---|
   | simd | `flashraw_simd.wasm` | stable, `+simd128` | — |
   | fallback | `flashraw.wasm` | stable | — |
   | threads | `flashraw_simd_threads.wasm` | nightly, `+atomics,+bulk-memory,+mutable-globals,+simd128`, `-Z build-std=panic_abort,std`, `--shared-memory --import-memory --max-memory=4 GiB` + TLS exports | `threads` (rayon) |
   | gpu | `flashraw_simd_gpu.wasm` | stable, `+simd128` | `gpu` (wgpu → WebGPU) |
   | simd-jxl | `flashraw_simd_jxl.wasm` | as simd (the `jpegxl` feature needs Rust ≥ 1.88) | `jpegxl` (JPEG XL DNG) |
   | threads-jxl | `flashraw_simd_threads_jxl.wasm` | as threads | `threads,jpegxl` |

   `[profile.wasm-release]`: opt-level 3, fat LTO, 1 codegen unit, `panic = "abort"`, stripped.
   The threaded variant runs `rustup run nightly cargo ...` (needs the nightly `rust-src`
   component; `FLASHRAW_NIGHTLY` picks another toolchain).
2. `wasm-bindgen --target web --omit-default-module-path` with the CLI version pinned to
   `Cargo.lock` (the script installs the matching `wasm-bindgen-cli` into `npm/.tools/` if needed).
   The glue of simd and fallback must be byte-identical (asserted), so one `flashraw_bindgen.js`
   serves both; threads and gpu have their own (`flashraw_bindgen_threads.js` takes a shared
   memory, `flashraw_bindgen_gpu.js` carries the WebGPU bindings) and are loaded with dynamic
   `import()` only when selected. The JPEG XL variants export the same ABI as their base build
   (asserted: the glue may only differ in the initial memory size), but ship their glue as
   separate files (`flashraw_bindgen_jxl.js`, `flashraw_bindgen_threads_jxl.js`): wasm-bindgen
   glue holds its instance in module state, and the JPEG XL instance lives next to the primary
   one. Thread workers of the threaded JPEG XL pool reuse `flashraw_bindgen_threads.js` (they are
   always handed their memory).
3. `wasm-opt -O3` (binaryen from npm devDependencies; `--enable-threads` for the threaded one).
4. Copies the hand-written JS/`.d.ts` into `dist/` and prints the size report.

`--variants simd,threads` builds a subset (dev only); `--no-opt` skips wasm-opt.

Runtime selection (`init()`), first match wins:

1. `init({ gpu: true })` → `flashraw_simd_gpu.wasm` (needs SIMD).
2. Threads wanted (default; `init({ threads: false })` / `FLASHRAW_WASM_THREADS=0` opt out), SIMD
   available and the platform can share memory with workers — browsers: `crossOriginIsolated`
   and `SharedArrayBuffer`; Node: `worker_threads` + shared `WebAssembly.Memory` — →
   `flashraw_simd_threads.wasm`. If its workers fail to start (CSP, missing worker file, 30 s
   timeout) the loader falls back to step 3 and records why.
3. `WebAssembly.validate()` of a 31-byte SIMD probe module picks `flashraw_simd.wasm`, otherwise
   `flashraw.wasm`; if the SIMD binary fails to compile, the loader retries with the fallback.
   Override with `init({ simd: false })` or `FLASHRAW_WASM_SIMD=0` (Node).

With `init({ jpegxl: 'always' })` steps 2 and 3 load the JPEG XL flavour of the threaded / SIMD
build instead (see JPEG XL below).

`init()` honours only its first options; `reinit(options)` selects another build later (e.g.
`{ gpu: true }` after the threaded default) by the same rules. Handles opened before keep their
build (see Engines below); new handles use the new one. Binaries are never unloaded: wasm-bindgen
glue holds its instance in module state, an ES module cannot be unloaded and WebAssembly memory
never shrinks, so every build used stays in memory (threaded: with its workers) for the life of
the page and is reused, without a new download or new workers, when selected again. The SIMD and
scalar binaries share `flashraw_bindgen.js`, so once one of them is instantiated `reinit` refuses
the other with `NotAvailableError` (a reload is the only way to swap them); a refused or failed
`reinit` leaves the previous selection in place. Calls are serialised.

`getCapabilities()` reports the choice: `wasm.binary`, `threads.{count, reason}`,
`gpu.{compiled, available, reason, adapter}`, `jpegxl.{mode, loaded, binary, reason}`.
Browsers use `fetch` + `instantiateStreaming`
(compile while downloading; the threaded module is compiled once and shared with its workers);
Node reads the file with `fs`.

### Threads

The threaded binary is the same Rust code with rayon enabled in every decoder (`flashraw/rayon`;
`flashraw-core::par` helpers become parallel), running on a thread pool implemented in the
package (no extra dependency; `npm/src/threads.js`, `crates/flashraw-wasm/src/lib.rs` `threads`):

```
 main thread                  coordinator worker                 N rayon workers
 ───────────                  ──────────────────                 ───────────────
 compile module, instantiate  instantiate (same module,          started at the same time:
 with shared memory           same memory)                       instantiate (same module,
                                                                 same memory)
 ── job 'build' ────────────▶ threadPoolBuild(n, onSpawn):
                              rayon calls onSpawn per thread ──▶ 'run' ptr to the next worker:
                              (blocks until all are primed)       threadRun(ptr)  (never returns)
 ── job 'open'/'decode' ────▶ jobOpen / jobDecode (may block,
    (await, never blocks)     rayon join/par_iter fan out) ────▶ work stealing
 ◀─ frame pointer ──────────  
 frameFromRaw(ptr): copy out / zero-copy view as usual
```

- **Why a coordinator**: browsers forbid blocking on the main thread (`Atomics.wait` and
  `memory.atomic.wait` trap there), and rayon blocks the calling thread while it waits for its
  workers — even `ThreadPoolBuilder::build_global` blocks until the pool is primed. So the main
  thread only allocates input buffers, copies results and frees memory (the allocator lock is a
  spin lock) and every call that may touch rayon (`open`, `decode`, `decodeTile`, RGBA previews)
  is a job on the coordinator. `decode()` stays `async`; the UI never stalls.
- **Ownership across threads**: objects cross as `Box` pointers (`InputBuffer.intoRaw` →
  `jobOpen` → `rawFileFromRaw`; `RawFile.share()` gives each job its own `Arc` so `close()`
  during a decode is safe; `jobDecode` → `frameFromRaw`). The JS layer is the only producer
  and consumer and uses each pointer exactly once.
- **Failure handling**: a Rust panic aborts the thread that hit it. In the coordinator the job
  fails with `InternalError` (panic message attached) as in the single-threaded build; in a rayon
  worker the job could never finish, so the worker reports the trap and the pool is poisoned
  (pending and later calls reject with `InternalError`). Startup problems (no worker support,
  CSP, a 404 on the worker script, 30 s timeout) fall back to the single-threaded build with the
  reason in `getCapabilities().threads.reason`.
- **Node**: same protocol over `worker_threads`; workers are `unref()`ed (the coordinator is
  `ref()`ed only while a job is pending), so an idle pool never keeps the process alive.
- **Browsers**: requires cross-origin isolation (COOP `same-origin` + COEP `require-corp` or
  `credentialless`), which is what exposes `SharedArrayBuffer`; see the npm README for the
  header setup in Vite/webpack. Firefox, Chrome and Safari ≥ 15.2 support shared WASM memory.
  `scripts/serve.mjs --isolated` serves the headers for local testing.
- **Thread count**: `navigator.hardwareConcurrency` / `os.availableParallelism()` (capped at 64)
  rayon threads plus the coordinator; `initThreads(n)` / `init({ threads: n })` /
  `FLASHRAW_WASM_THREADS=n`. Each thread has its own stack (2 MiB) and TLS in the shared
  memory. The pool is created once per page/process.
- **Start-up never grows the memory**: before spawning the workers the main thread grows the
  heap by what they will allocate while starting (`threadReserve(n)`: their stacks, TLS and
  pool state) and frees it back to the allocator. When one thread grows a shared memory, V8
  updates the bounds that *other* threads check `memory.copy` / `memory.fill` against only when
  they next handle an interrupt, so a thread copying memory that another thread just allocated
  from a freshly grown range can trap ("memory access out of bounds") although the access is in
  bounds. With 2 MiB stacks allocated as each worker instantiates, the memory used to grow about
  once per worker while the first rayon threads were already running, which made a few percent
  of cold page loads fall back to the single-threaded build.
- **Memory**: the shared memory has a 4 GiB maximum and never shrinks, like the single-threaded
  build. Zero-copy views (`zeroCopy: true`) point into a `SharedArrayBuffer`; the default copied
  output is a regular `ArrayBuffer`.
- **Output** is bit-identical to the single-threaded build for every output (tested on all
  fixtures and five corpus formats).

The JS-level pool (`flashraw/worker` → `createWorkerPool({ size })`) remains for decoding several
files concurrently and works without cross-origin isolation: N Web Workers / `worker_threads`,
each with its own single-threaded instance (pool workers do not start nested thread pools unless
`init: { threads }` is passed). Measured: 8 files (fixtures + 4 corpus DNGs, rgba8) sequential
4149 ms → 4 workers 1535 ms (2.7×) in Node.

### JPEG XL

JPEG XL DNG support (compression 52546, the `jpegxl` feature: `jxl-oxide`) adds ~1.5 MiB raw /
~330 KiB brotli (+54 %) to a binary, so the default binaries are built without it and two more
variants carry it: `flashraw_simd_jxl.wasm` and `flashraw_simd_threads_jxl.wasm`.

- **Engines**: `core.js` holds an *engine* per instantiated binary (glue module, exports, thread
  pool, build info). Every handle remembers its engine; memory views, jobs and frames go through
  it. The primary engine is the one `init()` selected.
- **Detection** (`jpegxl: 'auto'`, the default): files are always opened by the primary engine.
  On the first `decode*` of a handle, `frameInfo.compression === 'jpeg-xl'` (the raw IFD's
  compression tag; secondary IFDs such as an enhanced DNG's original data do not count) means
  the file needs the codec: the JPEG XL engine is loaded (once per page / worker / process,
  shared by all handles; a failed load is not cached), the input bytes are copied from the
  primary engine's memory into it, the file is reopened there and the old copy freed (kept
  until `close()` if zero-copy previews point into it). Metadata, previews and thumbnails never
  load it. As a safety net, an `UnsupportedCompression` error about JPEG XL from the primary
  engine triggers the same switch and a retry.
- **Variant choice**: threaded primary → `threads-jxl` with its own coordinator + rayon workers
  (same count; falls back to `simd-jxl` if they fail to start); `simd` or `gpu` primary →
  `simd-jxl` (JPEG XL files then decode on the CPU, GPU requests fall back with a reason); the
  scalar build has no JPEG XL variant (`UnsupportedCompressionError` saying SIMD is required).
- **Modes**: `'always'` loads the JPEG XL variant as the primary engine (one binary, no switch);
  `'never'` keeps the small build only and rewrites the decoder's error into an
  `UnsupportedCompressionError` that names the option. Node: `FLASHRAW_WASM_JPEGXL`.
- **Reporting**: `frame.backend.binary` / `frame.backend.jpegxl` say which binary decoded a
  frame, `raw.binary` which one serves a handle, `getCapabilities().jpegxl` the mode and state.
- **Bundlers**: the JPEG XL `.wasm` files are literal `new URL(..., import.meta.url)` assets and
  their glue a dynamic `import()`, so Vite/webpack emit separate files fetched only on demand
  (tested). The bytes are fetched once; the threaded variant is compiled once and shared with
  its workers.
- **Output**: lossless JPEG XL is bit-identical to native; lossy JPEG XL is float math, and
  `jxl-oxide` uses AVX2/FMA kernels on native x86-64, so ~0.1 % of the samples of a lossy test file
  differed by one level from native x86-64 (WASM single-threaded and threaded agree exactly).

### GPU

The WebGPU backend (`flashraw-gpu` through wgpu's WebGPU backend) ships as the opt-in
`flashraw_simd_gpu.wasm`, loaded with `init({ gpu: true })`:

- **Why opt-in**: +185 KiB raw / +55 KiB brotli of WASM and +52 KiB (+9 KiB gzip) of JS glue
  over the SIMD build, and it is single-threaded (below); most pages never need it.
- **Async all the way**: the browser cannot block on the GPU, so the bindings export
  `gpuInit()` (adapter + device request, once) and `RawFile.decodeGpu(tile, options)` returning a
  `Promise` (wasm-bindgen-futures over `FlashRaw::decode_gpu_async`). The synchronous `decode`
  export is never used with the GPU on the web.
- `decode({ backend: 'gpu' | 'webgpu', output })` runs the GPU path: bit-packed payloads are
  unpacked on the GPU from the file bytes; entropy-coded files are decoded on the CPU (WASM,
  single-threaded), uploaded once, and developed in one fused kernel. Raw outputs of
  entropy-coded files skip the GPU entirely (the CPU decode is the result), as in native.
- **GPU-resident outputs**: `output: 'gpu-rgba8' | 'gpu-raw-u16' | 'gpu-normalized' |
  'gpu-linear-rgb'` resolve to a JS `GpuFrame` wrapping the Rust `GpuOutput` (which owns the
  `RawFrame` whose data is `RawPixelData::Gpu`): `buffer` is the `GPUBuffer` (`STORAGE |
  COPY_SRC`, may be larger than `byteLength`, RGBA8 packed one pixel per u32),
  `createTexture()` copies RGBA8 into a new `rgba8unorm` `GPUTexture` owned by the caller,
  `download()` reads back into a regular `DecodedFrame`, `free()` returns the buffer to
  FlashRaw's pool, `detachBuffer()` hands the buffer over for good (never recycled). Timings
  include waiting for the GPU (`wait_idle`), so resident wall times are real.
- **Device sharing**: FlashRaw creates the device; `getGpuDevice()` returns it (via wgpu's
  `Device::as_webgpu`) so renderers can bind outputs without copies. Accepting an application's
  `GPUDevice` is not possible with wgpu 30 (its WebGPU backend cannot wrap a foreign device:
  `as_webgpu` exists, the reverse does not; only textures can be imported).
- **Fallback**: without the GPU build, `navigator.gpu`, or an adapter (or on a `GpuUnavailable`
  error during decode) the default `fallback: 'cpu'` decodes on the CPU (`gpu-*` outputs become
  their host output) and sets `backend.fallbackReason`; `fallback: 'error'` throws
  `GpuUnavailableError`. `getCapabilities().gpu.available` requests the device on first call.
- **WebGPU only, no WebGL**: the kernels are compute shaders, which WebGL2 lacks (wgpu's WebGL2
  backend cannot run them), so the WebGPU build is not compiled with it. Browsers without WebGPU
  (or with `navigator.gpu` but no adapter, e.g. Firefox on macOS at the time of writing) get the
  CPU fallback below. Verified: Chrome 154 (Metal) and WebKit 26.6 (Metal) run the GPU path;
  headless Chromium on Linux with `--enable-unsafe-webgpu` gets a SwiftShader software adapter
  (correct but slow); Firefox 155 (Linux, macOS) and WebKit on Linux fall back to the CPU.
- **Not combined with threads**: decoding would run on the coordinator worker, but WebGPU
  objects cannot move between workers, so resident outputs would be unusable on the main thread
  (and `flashraw`'s pool `install` requires `Send` frames). `--features gpu,threads` is a
  compile error.
- Accuracy: GPU `rgba8` vs CPU `rgba8` differs by at most 2 levels on < 1 % of the bytes
  (float rounding; tested in Chrome/Metal).

## Sizes

`npm run build` output (wasm-bindgen 0.2.129, wasm-opt -O3, binaryen 132):

| Binary | Raw | gzip -9 | brotli -11 | Its glue JS (raw / gzip) |
|---|---|---|---|---|
| `flashraw_simd.wasm` | 2450 KiB | 829 KiB | 618 KiB | 24 / 4 KiB (shared) |
| `flashraw.wasm` (fallback) | 2408 KiB | 811 KiB | 604 KiB | 24 / 4 KiB (shared) |
| `flashraw_simd_threads.wasm` | 2527 KiB | 822 KiB | 610 KiB | 30 / 5 KiB |
| `flashraw_simd_gpu.wasm` | 2636 KiB | 900 KiB | 672 KiB | 76 / 13 KiB |
| `flashraw_simd_jxl.wasm` | 3920 KiB | 1321 KiB | 951 KiB | 24 / 4 KiB |
| `flashraw_simd_threads_jxl.wasm` | 3989 KiB | 1313 KiB | 943 KiB | 30 / 5 KiB |
| All JS (5 glues + wrappers + workers, unminified) | 272 KiB | — | — | |

Sizes include every format decoder (measured after all vendor decoders were merged; the
DNG-only build was 216 KiB brotli). See [benchmarks.md](benchmarks.md#wasm) for the size
breakdown. Only one binary is downloaded per page (and only its glue: the threads/gpu glues are
dynamic imports), plus a JPEG XL variant only when a JPEG XL DNG is decoded (+333 KiB brotli
over the binary it complements: +54 %). The fallback and WebGPU builds have no JPEG XL flavour:
engines without SIMD predate JPEG XL DNGs, and a WebGPU one would be one more ~950 KiB brotli
file for a CPU-bound codec (the WebGPU build uses `flashraw_simd_jxl.wasm` instead). The
threaded binary costs nothing extra over the SIMD one (+77 KiB raw, −8 KiB
brotli: rayon is small, and std rebuilt with `build-std` drops some code). Of the SIMD binary,
416 KiB is code and 364 KiB data (mostly the camera database tables, which compress well).
`wasm-opt -Oz` instead of `-O3` saves < 2 %. `npm pack`: 6.2 MB tarball (2.7 MB of it the two JPEG XL
variants), 27 files.

### Colour management in the browser

`decode({ colorSpace: 'display-p3' })` (also `'adobe-rgb'`, `'prophoto'`, `'rec2020'`) develops
to another colour space at no extra per-pixel cost, and `frame.iccProfile` (a `Uint8Array`) is
the matching ICC profile for tagging a PNG/JPEG you encode. `outputProfile` (the bytes of a
`.icc` file, with `intent` and `blackPointCompensation`) converts to an arbitrary RGB profile;
profiles with lookup tables take a slower second pass and only the display outputs
(`rgba8`, `rgb16`). All binaries include the pure-Rust colour engine behind `outputProfile`
(cargo feature `icc` of `flashraw-wasm`, on by default): it costs about 86 KiB brotli-compressed
per binary, the built-in spaces about 8 KiB. Build with `--no-default-features` to drop it
(`outputProfile` then rejects with `InvalidArgumentError: built without ICC support`). The WebGPU
build handles the built-in spaces on the GPU; `outputProfile` decodes use the CPU. See
[color-management.md](color-management.md).

## Init latency

Cold process / fresh page, import + fetch/read + compile + instantiate:

| Environment | SIMD | Fallback | Threaded (incl. starting the workers) |
|---|---|---|---|
| Node 26 | 17–30 ms | 17–27 ms | 75–120 ms (16 threads) |
| Chromium 153 (headless, localhost) | 5–20 ms | 4–17 ms | 50–75 ms (16 threads) |
| Node 24, M1 Ultra | 23 ms | — | 88 ms (20 threads) |
| Chrome 154, M1 Ultra | 16 ms | 14 ms | 66 ms (20 threads) |

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
  unpack kernels. These tables are single-threaded WASM; the threaded build (next section)
  closes most of the multi-core gap.
- **SIMD128 vs scalar WASM**: identical output (tested); a clear win only where explicit WASM
  SIMD kernels exist today — the packed-bit unpack (`flashraw-codecs/src/packed.rs`): 12-bit
  Sigma fp 2.2–3.3 ms vs 5.6–7 ms (~2×). Huffman/LJPEG and the develop pipeline have no SIMD128
  kernels yet, so both binaries are within noise there. Porting the develop (demosaic/colour)
  pass to SIMD128 is the largest remaining WASM win (it is 80–90 % of rgba8 time).
- **Node vs Chromium**: same V8 engine, same results within noise.
- **Lossy DNG** spends its time in the JPEG codec (zune-jpeg); WASM is ~2.5× native there.

### Apple M1 Ultra: threads and WebGPU

Apple M1 Ultra (20 cores: 16 performance + 4 efficiency, 64-core GPU), macOS 27, Chrome
154 headless (`--enable-unsafe-webgpu`, WebGPU on Metal; adapter info is redacted by the
browser), Node 24.21. `npm run bench -- --browser --gpu --iterations 5 --no-fallback --chrome
<Chrome> --chrome-args --enable-unsafe-webgpu <files>`: median of 5 after 2 warm-ups; WASM =
wall time of `decode()` incl. the copy out of WASM memory; native = `flashraw benchmark`
(release build on the same Mac), `--threads 1` and all cores. 1T = single-threaded SIMD build,
20T = threaded build with 20 rayon threads. The machine was otherwise idle.

raw-u16 (ms):

| File | MP | Codec | Node 1T | Node 20T | Chrome 1T | Chrome 20T | Native 1T | Native 20T |
|---|---|---|---|---|---|---|---|---|
| Canon EOS R5 CR3 | 46.7 | CRX | 398 | 107 | 377 | 104 | 319 | 80 |
| Nikon Z 7 NEF (14-bit lossless) | 45.7 | Nikon Huffman | 350 | 343 | 289 | 272 | 193 | 193 |
| Sony A7R III ARW | 42.6 | uncompressed | 8.9 | 8.0 | 12.9 | 12.8 | 3.6 | 2.2 |
| Panasonic S1R RW2 | 47.6 | Panasonic v6 | 144 | 18 | 145 | 21 | 116 | 9.7 |
| Fujifilm GFX 100 RAF | 103.4 | Fuji compressed | 2127 | 163 | 1916 | 155 | 1429 | 100 |

rgba8 (decode + demosaic + colour + sRGB, ms); WebGPU = the WebGPU build in Chrome (CPU decode
single-threaded for entropy-coded files, GPU unpack for the uncompressed ARW, GPU develop):

| File | Node 1T | Node 20T | Chrome 1T | Chrome 20T | WebGPU readback | WebGPU resident | Native 1T | Native 20T |
|---|---|---|---|---|---|---|---|---|
| Canon EOS R5 CR3 | 898 | 149 | 835 | 152 | 422 | 384 | 751 | 111 |
| Nikon Z 7 NEF | 834 | 385 | 767 | 326 | 335 | 299 | 635 | 225 |
| Sony A7R III ARW | 428 | 50 | 458 | 62 | 49 | **15** | 408 | 33 |
| Panasonic S1R RW2 | 656 | 68 | 632 | 73 | 190 | 153 | 569 | 42 |
| Fujifilm GFX 100 RAF | 3160 | 251 | 2941 | 258 | 2090 | 1937 | 2404 | 173 |

Findings:

- **Threads**: 3.7–13× over single-threaded WASM where the codec is parallel (CRX planes,
  Panasonic blocks, Fuji strips; rgba8 adds the parallel demosaic/colour pass: 6–13×), landing
  at 1.3–2.2× native all-core time (rgba8: 1.3–1.9×). Sequential codecs (Nikon Huffman) only
  gain in the develop stage (2.2–2.4×). Raw uncompressed is memory-bound either way. Node and
  Chrome behave the same.
- **WebGPU**: the develop kernel itself takes 5–24 ms (GPU timestamps) for 43–103 MP, versus
  ~450–1000 ms single-threaded on the CPU. End to end it wins when the input is not
  entropy-coded: the uncompressed ARW goes file bytes → GPU unpack → develop → resident RGBA8 in
  15 ms (4× faster than 20 CPU threads, 2× faster than native 20T); reading the 170 MB of RGBA
  back costs ~34 ms. For entropy-coded files the WebGPU build is limited by its single-threaded
  CPU decode (it cannot be combined with threads, see GPU above): 1.5–3.4× faster than
  single-threaded WASM, on par with the threaded build for the sequential Nikon codec, slower
  than it otherwise.
- Choosing: default (threaded where isolated) for general use; the WebGPU build when the image
  goes to the GPU anyway (`gpu-rgba8` → `createTexture()`), especially for uncompressed/packed
  formats or isolation-less pages where threads are unavailable.

### Node: WASM instead of a native addon

Node uses the same WASM module as browsers. Rationale:

- **No install-time native build or per-platform prebuilt binaries** (N-API × OS × arch × libc),
  no postinstall scripts, works in serverless, StackBlitz, Deno, Bun and Electron alike; a single
  ~600 kB tarball.
- **Sandboxed**: untrusted RAW files are parsed inside WASM linear memory; a decoder bug cannot
  corrupt the Node process.
- **Cost**: 1.2–1.6× native single-thread time per decode (table above); with the threaded
  build (Node default) 1.3–1.9× native all-core time on entropy-coded 46–103 MP files (M1 Ultra
  table). For
  many files, `flashraw/worker` adds throughput across files (2.7× with 4 workers).

## Tests

| Command | What |
|---|---|
| `cargo test -p flashraw-wasm` | option parsing, structured errors, DNG round trip (native) |
| `npm test` | Node, threaded build by default (`FLASHRAW_WASM_THREADS=0 npm test` runs everything single-threaded): metadata/decode/preview vs native CLI hashes (4 bundled CC0 fixtures, 11 corpus DNGs when `$FLASHRAW_CORPUS` is present), all outputs, regions/tiles, input types, zero-copy, GPU fallback, garbage/truncated/corrupted input never traps, limits, memory release + GC reclamation, SIMD vs fallback equality, worker pool; `threads.test.mjs`: threaded vs single-threaded binaries identical for raw-u16/rgba8/half-size/linear-rgb/tiles on the fixtures + 5 corpus formats (CR3, NEF, ARW, RAF, RW2), raw-u16 == native `hash_full`, concurrent decodes, `close()` during a decode, errors keep the pool usable |
| `npm run test:types` | `tsc` against the `.d.ts` (bundler and NodeNext resolution) |
| `jpegxl.test.mjs` (in `npm test`) | JPEG XL DNGs (two corpus files carrying compression 52546 — one in its raw IFD, one only in a secondary IFD — and real-world samples from `$FLASHRAW_REALWORLD`): native hash equality (lossless; lossy within ±1 on < 0.5 % of samples), `auto` loads `simd-jxl` / `threads-jxl` exactly once and only for JPEG XL files (metadata/previews never), `always` / `never` / scalar-build behaviour, concurrent decodes, `close()` during the switch, worker pool. Without samples only the fixture checks (`always` mode, no load for other files) run |
| `npm run test:browser` | headless Chromium by default, `FLASHRAW_BROWSERS=chromium,firefox,webkit` for all three Playwright engines; two servers: cross-origin isolated → threaded binary selected, raw-u16 hash == native, RGBA8 `ImageData` on a canvas, single-threaded (`threads: false`) and scalar builds agree; not isolated → single-threaded with the reason; WebGPU build: GPU vs CPU rgba8 (±2), outputs changed by DNG opcodes decoded on the CPU, `gpu-rgba8` buffer/texture/download/detach when an adapter exists (Chromium runs with `--enable-unsafe-webgpu`: a SwiftShader software adapter on GPU-less Linux hosts; `FLASHRAW_CHROME=<Chrome>` for a hardware adapter, verified on Metal), and again with `navigator.gpu` removed: the CPU fallback and `GpuUnavailableError`; worker pool; errors; JPEG XL DNGs (corpus / `$FLASHRAW_REALWORLD`, when present): `auto` fetches `flashraw_simd_threads_jxl.wasm` (isolated) or `flashraw_simd_jxl.wasm` exactly once and only at the first JPEG XL decode, hashes == Node, pool workers load it too, `never` errors without fetching, `always` loads only the JPEG XL binary |
| `npm run test:bundlers` | `npm pack` → install into scratch apps → Vite 8, esbuild (browser + node), webpack 5 → run in Chromium; Vite and webpack served cross-origin isolated must run the threaded build from the bundle (all six `.wasm` and the thread worker emitted, the JPEG XL glue in its own chunk), esbuild served plain falls back; no JPEG XL `.wasm` is fetched before a JPEG XL DNG, then exactly one (`threads-jxl` isolated, `simd-jxl` otherwise) with the Node hash |

Hash check: `flashraw verify --json` prints `hash_full` = FNV-1a 64 over the LE bytes of the
u16 buffer; `tests/helpers.mjs` computes the same in JS (32-bit halves, no BigInt). Recorded
reference hashes live in `npm/tests/fixtures/expected.json` (regenerate with
`node tests/gen-expected.mjs`); tests prefer the live CLI when `target/release/flashraw` exists.

Chromium needs system libraries; without root, `npm/scripts/fetch-browser-libs.sh` downloads
and extracts them (the test runner picks them up automatically).
