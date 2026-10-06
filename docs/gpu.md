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
  src/run.rs       GpuFrame/GpuInput (what to upload), GpuOp (what to compute), GpuContext::run,
                   band planner (tiling) and the pipelined band loop
  src/image.rs     GpuImage (device-resident result, single buffer or tiles), download, to_texture
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

**Buffers.** Large allocations are pooled (≤ 1 GB kept). Every binding names an
explicit size, so a pooled buffer larger than the binding limit is still usable. On Apple Silicon a fresh
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
| develop → 16-bit sRGB (`Rgb16`, oriented), linear 16-bit, linear half floats, with and without exposure/black/white points | `process::develop_encoded` | one code / one half-float ULP on < 1 % of samples (Metal: ≤ 0.21 % synthetic, ≤ 0.07 % on real files; software Vulkan: 0) |
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
  `trunc(clamp(v, 0, 1) * 4096)`; the 16-bit encode interpolates the same 16385-entry curve
  table as the CPU.
* **Half floats** are converted with the CPU's integer algorithm (round to nearest even,
  saturating at ±65504), not `pack2x16float`, which needs an optional shader capability and
  leaves rounding to the implementation.
* **Levels** (`exposure`, `black_point`, `white_point`) apply after the matrix as
  `v * scale - offset`, like the CPU.

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
2. **Compute.** One dispatch per band. There is one band unless the frame exceeds the
   adapter's buffer limits (see [Tiling](#tiling-frames-beyond-the-adapter-limits)).
3. **Output.** Downloaded for `RawU16`, `NormalizedF32`, `LinearRgbF32`, `Rgba8`, `Rgb16`,
   `LinearRgb16` and `LinearRgbF16`. The three 16-bit RGB outputs are computed as 16-bit
   RGBA (two 32-bit words per pixel, so no two invocations share a word) and the fourth
   sample is dropped while copying out of the device buffer (`GpuImage::download_rgb`):
   the host never holds a 4-channel copy. They have no GPU-resident variant.
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
* `, N bands` appended to `backend.postprocess` when the frame was tiled.

## Tiling (frames beyond the adapter limits)

Frames of any size work. When the input or the output of a run does not fit in one
storage binding, `GpuContext::run` processes the frame in horizontal bands.

**Band planning** (`run.rs`, `BandGeometry::plan`). The budget per binding is
`min(max_storage_buffer_binding_size, max_buffer_size, 4 GB)` (32-bit addressing), or
the cap set with `GpuContext::override_limits`. A band is a range of *computed rows*:
frame rows for unpack/normalise, developed rows for develop. The planner picks the
largest row count whose input and output both fit the budget:

| Kernel | Input rows read for computed rows `[y0, y1)` | Band granularity |
|---|---|---|
| unpack, normalise | `[y0, y1)` | 2 rows when a row holds an odd number of u16 samples, so bands start on whole output words |
| develop (bilinear / 3x3 same-colour) | `[crop_y + y0 − 1, crop_y + y1 + 1)` (±1-row halo) | multiples of the 16-row workgroup tile |
| develop half-size | `[crop_y + 2·y0, crop_y + 2·y1)` (whole 2x2 blocks) | multiples of 16 rows |

Bands span the full width, so no column halo is needed. The number of rows per band is
also bounded by `max_compute_workgroups_per_dimension`.

**Coordinates stay global.** Each band gets its own `Params` (one uniform buffer,
256-byte stride). `width`/`height`, `origin`, the crop and the developed size are those
of the whole frame. The CFA phase, black pattern, row/column deltas and border handling
are therefore exactly those of an untiled run. Only two things are band-relative:

* **The source fetch.** `src_y` is the frame row of the first uploaded row; the kernel
  reads row `y − src_y` of the band's upload.
* **The store.** `band_y`/`band_end` select the rows the dispatch computes.
  `dst_x`/`dst_y`/`dst_stride`/`dst_off` locate the output rectangle bound at `dst`.

As a result, the banded output is **bit-identical** to the untiled one (tested).

**Input per band.** Each band uploads only the rows it needs:

* **Packed payloads:** bytes `[(win.y + in0) · row_stride, (win.y + in1 − 1) · row_stride +
  max(row_stride, row_bytes))`, clipped to the payload.
  * The slice is uploaded from the 4-byte-aligned address below its start, and
    `src_offset` skips the alignment bytes.
  * `src_len` keeps the "bytes past the payload read as 0" rule for truncated files.
  * Large packed files therefore still skip the CPU decode.
* **u16/f32 rasters:** whole raster rows `[win.y + in0, win.y + in1)`; `src_x` windows
  the columns.

Region decodes also benefit: they upload only the rows of the region.

**Output.** One of three layouts:

1. **Single binding** (output ≤ budget): one buffer, bound whole by every band.
2. **Sub-range binding** (output ≤ `max_buffer_size`, row bands): one buffer. Each band
   binds only the byte range it writes. The range starts at an offset aligned to
   `min_storage_buffer_offset_alignment`, and `dst_off` absorbs the remainder. The
   result is an ordinary single-buffer `GpuImage`.
3. **Tiled** (output > `max_buffer_size`, or rotated RGBA8 output > budget): one buffer
   per band, and the `GpuImage` is tiled (see below).

**Orientation.** A band of developed rows `[y0, y1)` covers this part of the RGBA8
output (`dw`, `dh` = developed size):

| EXIF orientation | Output rectangle |
|---|---|
| 1, 2 | rows `[y0, y1)` |
| 3, 4 | rows `[dh − y1, dh − y0)` |
| 5, 8 | columns `[y0, y1)`, full height |
| 6, 7 | columns `[dh − y1, dh − y0)`, full height |

Column bands are not contiguous in a row-major buffer. Rotated outputs larger than one
binding therefore use the tiled layout, with full-height column tiles.

**Tiled `GpuImage`.**
* **Shape:** `is_tiled()` is true, `buffer()` returns `None`, and `tiles()` returns the
  `GpuTile { rect, buffer }` list in image order. Tiles are disjoint and cover the image.
* **Tile layout:** each tile holds the pixels of `rect`, row-major, with
  `rect.width * channels` samples per row, starting at byte 0. The element format is
  the same as for an untiled image.
* **Your own pipeline:** bind each tile's buffer and offset your coordinates by
  `rect.x`/`rect.y`.
* **Built-in helpers:**
  * `download()` reassembles the frame on the host (contiguous copies for row tiles,
    row by row for column tiles);
  * `to_texture()` writes every tile into one texture (one dispatch per tile, offset
    by `rect`);
  * `into_tiles()` takes ownership of the buffers.

**Pipelining.** Bands alternate between two upload slots. The host fills the slot for
band N+1 while the GPU runs band N. Before mapping a slot, FlashRaw waits only for the
submission that last used it (band N−1), not for the whole queue. What a slot is
depends on the memory model:
* **Unified memory:** the storage buffer itself (mapped write).
* **Elsewhere:** a mapped staging buffer, copied into the storage buffer at the start
  of the band's command buffer; inputs under 1 MB use `write_buffer`.

Output, slot and staging buffers come from the pool. Timings with several bands:
* `GpuTimings::upload_ms` counts only host copies; waiting for a slot is part of
  `compute_ms`;
* `kernel_ms` sums the per-band GPU timestamps.

**Limit override.** `GpuContext::override_limits(max_binding, max_buffer)` caps the
binding and buffer sizes FlashRaw plans with (4 KB minimum). It bounds peak device
memory. The tests and the benchmark also use it to force tiling on adapters with large
limits.

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
gpu = overhead + (bands - 1) * band_overhead
      + upload_bytes / upload_bw + samples / gpu_develop + out_bytes / download_bw
use GPU if gpu < 0.85 * cpu
```

There is no size cut-off: frames beyond the adapter's limits run tiled.
* `bands` comes from `GpuContext::estimate_bands` (input and output size against the
  binding budget).
* `band_overhead` is measured by the calibration, which runs its frame in 8 bands and
  compares with one band.

* The **GPU numbers** come from a one-time micro-benchmark per context. It takes about
  100 ms on the M1 Ultra, including pipeline compilation, and runs:
  * a synthetic 2048x2048 12-bit packed frame through develop, half-size, unpack and
    normalise;
  * transfers at two sizes, to separate fixed cost from bandwidth. A transfer is the
    host copy plus the non-kernel part of the compute wall time, which includes device
    copies on discrete GPUs;
  * the 8-band run;
  * a download.
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
  * For tiled images (larger than one storage binding) `buffer()` returns `None`; use
    `GpuImage::tiles()` instead (see Tiling).
  * `GpuImage::to_texture()` produces an `Rgba8Unorm` texture
    (`TEXTURE_BINDING | STORAGE_BINDING | COPY_SRC`).
  * Buffers come from FlashRaw's pool and are recycled when the `GpuImage` is dropped.
    Keep it alive while the GPU uses the buffer, or take ownership with `into_buffer()`
    (untiled) or `into_tiles()`.
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

### Tiling (forced bands)

On the M1 Ultra, both the storage-binding limit and the buffer limit are 4.29 GB, so
no real frame needs tiling there. To measure what tiling costs, the bands were forced:
* `FLASHRAW_BENCH_BAND_MB` caps the binding size;
* `FLASHRAW_BENCH_TILED=1` caps the buffer size too, so resident outputs are tiled
  (one buffer per band).

Same command as above, median of 7, RAW→RGBA8 bilinear. Half-size behaves the same way
and is omitted.

| File | MP | Bands config | Bands | CPU ms | GPU ms (download) | GPU ms (resident) |
|---|---|---|---|---|---|---|
| Phase One IQ4 150MP (IIQ L 14) | 154.0 | natural (untiled) | 1 | 149.9 | 94.2 | 77.1 |
| | | 64 MB bindings, one output buffer | 10 | 149.7 | 82.8 | 74.0 |
| | | 64 MB, tiled output | 10 | 148.1 | 92.7 | 73.8 |
| | | 16 MB bindings, one output buffer | 37 | 144.3 | 88.4 | 81.3 |
| | | 16 MB, tiled output | 37 | 143.2 | 99.0 | 75.7 |
| Fujifilm GFX100S II (uncompressed) | 103.4 | natural (untiled) | 1 | 75.7 | 31.1 | 18.6 |
| | | 64 MB bindings, one output buffer | 7 | 75.8 | 31.6 | 14.6 |
| | | 64 MB, tiled output | 7 | 77.5 | 33.5 | 20.7 |
| | | 16 MB bindings, one output buffer | 25 | 76.7 | 31.8 | 15.1 |
| | | 16 MB, tiled output | 25 | 77.2 | 37.8 | 22.4 |
| Hasselblad X2D 100C (3FR) | 105.3 | natural (untiled) | 1 | 79.8 | 44.1 | 28.9 |
| | | 64 MB bindings, one output buffer | 7 | 81.5 | 38.3 | 27.6 |
| | | 64 MB, tiled output | 7 | 79.4 | 39.4 | 32.3 |
| | | 16 MB bindings, one output buffer | 25 | 80.6 | 42.6 | 33.9 |
| | | 16 MB, tiled output | 25 | 77.8 | 47.7 | 31.7 |

Two more large files at natural limits (no tiling), RAW→RGBA8:

| File | MP | Payload | CPU ms | GPU ms (download) | GPU ms (resident) |
|---|---|---|---|---|---|
| Sony ILCE-7RM5 uncompressed | 61.2 | packed 14-bit | 44.1 | 17.2 | 11.1 |
| Light L16 linear DNG | 64.6 | 3 planes, uncompressed | 51.5 | 32.0 | 24.8 |

Reading the numbers:

* **Banding costs little.** With one output buffer and sub-range bindings, 7–37 bands
  are within run-to-run noise of the untiled run, and sometimes faster. Two effects
  explain it: the upload of band N+1 overlaps the kernel of band N, and the input
  slots are band-sized instead of frame-sized.
* **Tiled outputs cost more to read back.** Separate per-band buffers add 2–8 ms to
  100–150 MP downloads (one map per tile), and the summed per-band kernel timestamps
  are higher. FlashRaw only tiles the output when it must, so prefer adapters or caps
  where `max_buffer_size` allows the single-buffer layouts.
* **Two of these files include a CPU decode in every column.**
  * The IQ4 payload is entropy-coded.
  * The 3FR payload is packed, but its black level is measured from masked pixels, so
    the CPU decodes it first.
  * The GFX100S II payload is unpacked on the GPU.
* **No discrete GPU was available to measure.** On a discrete GPU, each band uses the
  staging path: a mapped staging buffer plus a device copy. Overlapping the PCIe upload
  of band N+1 with the kernel of band N should matter more there than on unified
  memory, but this is unmeasured.

## Limitations

* **Rendering scope:** only bilinear / 3x3 same-colour / superpixel demosaic and a
  3x3 matrix are implemented, matching the CPU `process` module. There is no highlight
  recovery, noise reduction or lens correction.
* **Interop requires a shared device:** outputs live on the device of the context that
  produced them. Use `GpuContext::from_device` for zero-copy interop with your own
  device.
* **Buffer-size limits:** frames beyond the adapter's binding/buffer limits are tiled
  (see Tiling).
  * Only a band row larger than the binding budget is rejected
    (`Error::LimitExceeded`); that would take a row of about 1 GB.
  * Indices are 32-bit *per band*, so one band uploads at most 4 GB. Whole frames have
    no such limit.
  * `GpuImage::to_texture` is still bounded by `max_texture_dimension_2d`.
* **Tiled outputs are row or column bands only** (no 2-D tiles): a band spans the full
  width of the output, or the full height for rotated RGBA8.
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
