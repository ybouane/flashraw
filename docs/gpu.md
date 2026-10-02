# GPU backend

FlashRaw's optional GPU backend (`flashraw-gpu`, enabled with the `gpu` feature of
`flashraw`) runs the post-decode pipeline as wgpu compute shaders. It works on Vulkan,
Metal, DX12 and GL natively, and on WebGPU in the browser.

It does **not** replace the decoders. Entropy decoding (lossless JPEG, vendor Huffman
coders, …) is sequential and stays on the CPU. The GPU takes over where the work is
data-parallel:

* unpacking uniformly bit-packed payloads, read straight from the file bytes;
* black/white normalisation;
* the fused develop: demosaic → white balance → colour matrix → sRGB → orientation.

## Architecture

```
crates/flashraw-gpu/
  src/context.rs   adapter/device management (lazy, cached, async + blocking), adapter class,
                   pipeline cache (override-specialised), buffer pool, error capture
  src/run.rs       GpuFrame/GpuInput (what to upload), GpuOp (what to compute), GpuContext::run
  src/image.rs     GpuImage (device-resident result), download, to_texture
  src/kernels.rs   Params block (mirrors WGSL), calibration/CFA/sRGB tables, scalar reference model
  src/policy.rs    cost model for Backend::Auto + one-time GPU calibration
shaders/           WGSL sources (shared by native and browser builds, include_str!'d)
  common.wgsl      bindings, Params, packed bit extraction, linearisation, normalisation
  unpack.wgsl      packed/u16 → u16 (+ LUT)
  normalize.wgsl   → black-subtracted, white-normalised f32
  develop.wgsl     fused develop (bilinear `develop`, 2x2 `develop_half`)
  to_texture.wgsl  RGBA8 buffer → rgba8unorm storage texture (interop)
crates/flashraw/src/gpu.rs   facade: backend selection, GPU decode path, RawFrame::download
```

**Device management.** `flashraw_gpu::context()` creates the global context on first
use (native: blocking via `pollster`). `init().await` does the same asynchronously and
is required in the browser. The context requests the high-performance adapter with the
adapter's own limits (a 60 MP RGBA8 frame is 240 MB, beyond WebGPU's defaults).
It also requests:

* `TIMESTAMP_QUERY`, when available, for kernel timings;
* `MAPPABLE_PRIMARY_BUFFERS` on integrated/unified-memory adapters.

Device errors never panic. They are captured with error scopes and an
uncaptured-error handler, and returned as `Error::GpuUnavailable`. Set
`FLASHRAW_GPU=off` to disable the backend; `WGPU_BACKEND` selects a wgpu backend.

**Pipelines** are specialised with WGSL `override` constants (input mode, pack order,
LUT, CFA layout, output mode), so per-file branches are compiled out. They are cached
per context.

**Buffers.** Large allocations are pooled (≤ 1 GB kept). On Apple Silicon a fresh
200 MB buffer costs several milliseconds of page mapping and zero-fill: before pooling,
"kernel" time for a 47 MP develop was 16.6 ms, and with pooling 5.8 ms.
* **Unified memory** (`MAPPABLE_PRIMARY_BUFFERS`): input is written straight into the
  storage buffer and output is mapped in place, with no staging copies.
* **Elsewhere:** one reused mapped staging buffer is used per direction.

Downloads copy out of the mapping with several threads, because the fresh destination
`Vec` page-faults.

## Kernels

Every kernel has a CPU scalar reference and an equivalence test
(`crates/flashraw-gpu/tests/equivalence.rs`). GPU tests print `skipped` and pass when
no adapter exists.

| Kernel | Reference | Equivalence |
|---|---|---|
| packed unpack: every `PackOrder` (MSB bytes, LSB bytes, MSB16, MSB32) × 1..16 bits, arbitrary/unaligned row stride, truncated payloads, windows | `flashraw_codecs::packed::unpack_row_scalar` per row | **bit-exact** (768 GPU cases + the scalar model across all depths/orders/strides, which also runs without a GPU) |
| linearisation LUT (`v → lut[min(v, len-1)]`, fused into fetch) | DNG decoder's curve application | **bit-exact** |
| normalise (per-position black pattern = lcm(CFA, black repeat), row/col deltas, per-colour white, origin-anchored) | `process::normalize` | ≤ 1e-5 (measured: 0) |
| develop (bilinear Bayer, 3x3 same-colour for X-Trans/other CFAs, RGB planes, mono; WB, clip, matrix) → linear f32 | `process::develop` | ≤ 1e-4 (measured ≤ 3.6e-7) |
| develop → sRGB RGBA8 with all 8 EXIF orientations | `process::develop_rgba8` | ±1 on < 0.1 % of samples (synthetic: 0; real files ≤ 0.0002 %) |
| half-size 2x2 superpixel | `process::develop` (HalfSize) | same as above |

The GPU path reproduces the CPU semantics exactly:
* **Bilinear demosaic:** a site's own colour is the centre sample. Each other colour is
  the mean of the same-colour samples in the 3x3 neighbourhood; at borders only
  in-bounds samples count.
* **Same arithmetic as the CPU:**
  * interior sites multiply by a host-computed `1/count`, as in the CPU fast path;
  * Bayer sums in the same order as the CPU fast path;
  * white balance, then clip, then the matrix.
* **sRGB encode** uses the same 4097-entry LUT, indexed by
  `trunc(clamp(v, 0, 1) * 4096)`.

Remaining differences are FMA/division rounding of a few ULP in f32.

**Packed addressing.** The payload is uploaded as u32 words. A sample at bit
`i * bits` of row `y` is assembled from up to 3 logical bytes:
* `MSB16`/`MSB32` read physical byte `k ^ 1` / `k ^ 3`, relative to the row start;
* bytes past the row's data or the payload read as 0. These are the semantics of the
  CPU bit pumps reading a row slice, which also makes truncated files decode like the
  CPU.

For byte orders there is a fast path: a two-word fetch plus a funnel shift.

**Fusion.** `develop` reads packed bits, u16 or f32 directly. Each 16x16 workgroup
stages its 18x18 neighbourhood of *normalised* samples in workgroup memory, then
demosaics. The steps run in one dispatch with no full-size intermediates:
unpack → linearise → normalise → demosaic → WB → clip → matrix → sRGB → orientation →
store.

## Data path

`DecodeOptions { backend: Gpu | Auto, output }`:

1. **Upload.**
   * If the format exposes `packed_payload()` (e.g. uncompressed DNG), the file bytes
     are uploaded and unpacked on the GPU. Region decodes window into the payload.
   * Otherwise (entropy-coded payloads, black level measured from masked pixels, float
     data), the CPU decodes and the u16/f32 samples are uploaded once.
2. **Compute.** One dispatch.
3. **Output.** Downloaded for `RawU16`, `NormalizedF32`, `LinearRgbF32` and `Rgba8`.
   The `Gpu*` variants (`GpuRawU16`, `GpuNormalizedF32`, `GpuLinearRgbF32`, `GpuRgba8`)
   leave it on the device and return `RawPixelData::Gpu(Arc<GpuImage>)`.
   `RawFrame::download()` / `download_async()` convert later.

`DecodeStatistics` reports:
* `GpuUpload`, `GpuCompute` and `GpuDownload` stage times (wall clock, each
  synchronised);
* `backend.gpu_adapter`;
* `backend.postprocess`, e.g. `GPU Apple M1 Ultra (Metal, integrated), kernel 5.80 ms [auto: …]`.
  The kernel time comes from GPU timestamps when supported.
* `backend.decode`: `GPU unpack (…)` when the GPU unpacked the payload.

## Backend selection (`Backend::Auto`)

Hard rules first:

* `RawU16`/`RawF32` outputs never touch the GPU. They are transfer-bound: the GPU loses
  even for packed payloads (table below). This also means the default `decode()` never
  initialises a GPU.
* Frames < 2 MP stay on the CPU (fixed overheads dominate).
* Software adapters (llvmpipe, lavapipe, SwiftShader, WARP) are never auto-selected.
* `Gpu*` (resident) outputs always use the GPU.
* In the browser, synchronous `decode()` stays on the CPU; use `decode_gpu_async`.

Otherwise a cost model compares estimated end-to-end times:

```
cpu = [samples / cpu_unpack if packed] + pixels / cpu_develop        (or normalize / half rates)
gpu = overhead + upload_bytes / upload_bw + samples / gpu_develop + out_bytes / download_bw
use GPU if gpu < 0.85 * cpu
```

* The **GPU numbers** come from a one-time micro-benchmark per context: a synthetic
  2048x2048 12-bit packed frame through develop, half-size, unpack and normalise;
  upload at two sizes to separate fixed cost from bandwidth; download. It takes about
  80 ms on the M1 Ultra, including pipeline compilation.
* The **CPU numbers** come from timing the CPU kernels on a 2048x1024 frame (about
  15 ms, cached per thread count).
* `FLASHRAW_GPU_CALIBRATION=static` uses conservative per-class defaults instead.
* `flashraw::gpu::calibrate()` runs both calibrations up front, and
  `flashraw::gpu::auto_decision(&raw, &opts)` explains a decision.

One-time costs in a fresh process (M1 Ultra): device creation ≈ 30 ms, GPU calibration
≈ 80 ms, CPU calibration ≈ 15 ms. Auto pays these only for RGB/normalised outputs of
frames ≥ 2 MP. A one-shot CLI conversion therefore may not benefit; long-running
applications do.

## Interop

* **Native wgpu.** Hand FlashRaw your device with
  `flashraw_gpu::set_context(Arc::new(GpuContext::from_device(device, queue, &adapter_info)))`.
  Outputs are then created on your device.
  * `GpuImage::buffer()` is a `STORAGE | COPY_SRC` buffer (plus `MAP_READ` on unified
    memory). RGBA8 is one `u32` per pixel (`R | G<<8 | B<<16 | A<<24`); f32 RGB is
    3 floats per pixel; u16 is two samples per word.
  * `GpuImage::to_texture()` produces an `Rgba8Unorm` texture
    (`TEXTURE_BINDING | STORAGE_BINDING | COPY_SRC`).
  * Buffers come from FlashRaw's pool and are recycled when the `GpuImage` is dropped.
    Keep it alive while the GPU uses the buffer, or take ownership with `into_buffer()`.
    `flashraw_gpu::wgpu` re-exports the wgpu version used.
* **WebGPU (browser).** The crate builds for `wasm32-unknown-unknown`
  (`cargo check -p flashraw-gpu --target wasm32-unknown-unknown`, and `flashraw` with
  `--no-default-features --features gpu,simd`). The same WGSL runs through wgpu's
  WebGPU backend.
  * Call `flashraw::gpu::init().await`, then `raw.decode_gpu_async(opts).await`
    (synchronous waits are impossible on the main thread).
  * Downloads are `download_async`.
  * `flashraw_gpu::wgsl_sources()` exposes the kernel sources for hand-written JS
    pipelines.

## Benchmarks

Apple M1 Ultra (20 CPU threads, 64-core GPU, Metal, unified memory).
`cargo bench -p flashraw-gpu --bench gpu_pipeline`, median of 7 decodes after one
warm-up. Times are wall-clock for `FlashRaw::decode` (file already opened/mmapped).

The CPU side is FlashRaw's own SIMD + multithreaded path. Both sides do the same work:
the identical bilinear algorithm, checked by the equivalence tests.
* **GPU (download):** upload + kernel + copy back into a `Vec`.
* **GPU (resident):** the `Gpu*` outputs, which wait for completion but don't download.

Stages are per-run medians: upload / compute wall / kernel (GPU timestamps) / download.

| File | MP | Payload | Case | CPU ms | GPU ms (download) | GPU ms (resident) | Auto ms (choice) | GPU up / compute / kernel / down |
|---|---|---|---|---|---|---|---|---|
| Leica Q2 | 47.4 | packed 14-bit | unpack (raw u16) | **2.3** | 6.4 | 3.4 | 2.2 (CPU) | 2.6 / 1.4 / 0.9 / 2.4 |
| Leica Q2 | 47.4 | packed 14-bit | RAW→RGBA8 | 34.5 | 13.9 | **8.5** | 13.9 (GPU) | 2.7 / 6.3 / 5.8 / 4.9 |
| Leica Q2 | 47.4 | packed 14-bit | RAW→RGBA8 half | 23.7 | 6.9 | **5.3** | 6.4 (GPU) | 2.6 / 2.7 / 2.1 / 1.2 |
| Nokia Lumia 1020 | 38.3 | packed 10-bit | unpack (raw u16) | **2.2** | 5.4 | 2.5 | 2.1 (CPU) | 1.8 / 1.4 / 1.0 / 2.2 |
| Nokia Lumia 1020 | 38.3 | packed 10-bit | RAW→RGBA8 | 27.8 | 11.4 | **6.6** | 11.4 (GPU) | 1.8 / 5.2 / 4.8 / 4.3 |
| Nokia Lumia 1020 | 38.3 | packed 10-bit | RAW→RGBA8 half | 19.7 | 5.2 | **3.5** | 5.1 (GPU) | 1.9 / 2.2 / 1.7 / 1.1 |
| Leica CL | 24.6 | packed 14-bit | unpack (raw u16) | **1.4** | 4.0 | 2.2 | 1.4 (CPU) | 1.7 / 1.0 / 0.6 / 1.3 |
| Leica CL | 24.6 | packed 14-bit | RAW→RGBA8 | 19.2 | 7.7 | **4.8** | 7.7 (GPU) | 1.8 / 3.5 / 3.0 / 2.5 |
| Leica CL | 24.6 | packed 14-bit | RAW→RGBA8 half | 12.8 | 4.4 | **2.8** | 3.7 (GPU) | 1.4 / 2.3 / 1.8 / 0.7 |
| Leica M9 | 18.1 | packed 16-bit | unpack (raw u16) | **1.3** | 3.4 | 1.9 | 1.2 (CPU) | 1.6 / 0.8 / 0.3 / 1.1 |
| Leica M9 | 18.1 | packed 16-bit | RAW→RGBA8 | 13.8 | 6.5 | **3.8** | 6.4 (GPU) | 1.6 / 2.7 / 2.2 / 2.2 |
| Leica M9 | 18.1 | packed 16-bit | RAW→RGBA8 half | 9.9 | 3.8 | **2.4** | 3.0 (GPU) | 1.2 / 2.0 / 1.6 / 0.6 |
| Pentax 645Z | 52.0 | lossless JPEG | RAW→RGBA8 | 278.0 | 261.1 | **258.1** | 262.8 (GPU) | 3.6 / 12.5 / 11.9 / 5.4 |
| Pentax 645Z | 52.0 | lossless JPEG | RAW→RGBA8 half | 265.5 | 252.9 | **252.7** | 252.0 (GPU) | 3.3 / 4.8 / 4.2 / 1.7 |
| Pentax K-1 | 36.6 | lossless JPEG | RAW→RGBA8 | 177.6 | 168.9 | **163.8** | 166.4 (GPU) | 2.6 / 9.0 / 8.4 / 4.3 |
| Pentax K-1 | 36.6 | lossless JPEG | RAW→RGBA8 half | 168.2 | 157.5 | **157.2** | 158.1 (GPU) | 2.8 / 3.5 / 3.0 / 1.2 |

Reading the numbers:

* **Packed RAW → RGBA8: the GPU wins 2.5× with download and 4× resident.** The whole
  47 MP pipeline is 5.8 ms of kernel time, against 34.5 ms on 20 CPU threads.
* **Raw u16 unpack: the GPU loses.** The CPU unpacks 47 MP in 2.3 ms; that is memory
  bandwidth on a 20-core machine. The GPU needs 6.4 ms, of which upload + download are
  5 ms. Even resident (3.4 ms) it doesn't pay. Auto never uses the GPU for raw outputs.
* **Entropy-coded files: the GPU only shaves the post-processing.** Lossless-JPEG
  decoding (~150–250 ms here, single-stream) dominates, and the GPU saves 9–15 ms
  (5–6 %).
* **Kernel times after long CPU-only phases are ~2× higher than steady state.** On the
  LJPEG files, develop of 36.6 MP takes 8.4 ms in the table but 4.2 ms when back-to-back.
  Apple's GPU clocks down while the CPU decodes. Discrete GPUs behave similarly.
* **Benchmarks were not run on a discrete GPU or under WebGPU.** On PCIe the transfer
  stages are bounded by bus bandwidth (~25 GB/s); expect RAW → RGBA8 *with download*
  to be close to break-even and resident outputs to win. The calibration measures this
  and Auto adapts.
* `Backend::Cpu` vs `Backend::CpuSimd` only changes the unpack; the CPU develop is
  the same multithreaded fused pass for both (`flashraw benchmark --backend cpu
  --output rgba8` gives 33 ms for the Q2, `--backend gpu` 17 ms and `--backend auto`
  14 ms; each of those CLI numbers includes per-process warm-up).

## Limitations

* **Rendering scope:** only bilinear / 3x3 same-colour / superpixel demosaic and a
  3x3 matrix are implemented, matching the CPU `process` module. There is no highlight
  recovery, noise reduction or lens correction.
* **Interop requires a shared device:** outputs live on the device of the context that
  produced them. Use `GpuContext::from_device` for zero-copy interop with your own
  device.
* **Buffer-size limits:** frames larger than the adapter's `max_storage_buffer_binding_size`
  are rejected with `Error::LimitExceeded`; Auto falls back to the CPU in that case.
  There is no tiling yet. Indices are 32-bit, so payloads must be under 4 GB.
* **Half-size mono:** for half-size develop of single-plane non-CFA images, the GPU
  returns the grey average. The CPU reference currently returns black (it averages a
  channel it never accumulates), so that case is excluded from equivalence tests.
  2-plane images also differ in half-size.
* **Synchronous decode in the browser:** sync `decode()` cannot use the GPU in the
  browser.
* **No CI GPU:** the Linux CI machine has no GPU driver (no Mesa lavapipe installed).
  GPU tests skip there, and only the scalar reference model and offline naga validation
  of all WGSL run. Run `cargo test -p flashraw-gpu` on a machine with a GPU, or install
  `mesa-vulkan-drivers` (lavapipe) to exercise the kernels in software.
