# flashraw

<!-- flashraw-intro -->
<p align="center"><a href="https://flashraw.ybouane.com"><img src="https://raw.githubusercontent.com/ybouane/flashraw/main/branding/readme-banner.png" alt="FlashRaw: the fast, safe camera RAW decoder" width="100%"></a></p>

**[FlashRaw](https://flashraw.ybouane.com)** is a fast, memory-safe camera RAW decoder written in Rust, with Rust, C, C++, Python and JavaScript (WebAssembly) bindings. It decodes about 40 RAW formats (CR2, CR3, NEF, ARW, RAF, DNG, RW2, ORF, PEF and more) with SIMD kernels, multithreading and an optional GPU backend, converts to display-ready RGB with ICC colour management, and runs in the browser.

**Official website and live demo: [flashraw.ybouane.com](https://flashraw.ybouane.com)** (it decodes your file locally in the browser) · [Source on GitHub](https://github.com/ybouane/flashraw) · Follow [@ybouane on X](https://x.com/ybouane)
<!-- /flashraw-intro -->

Fast, safe camera RAW decoding for browsers and Node.js, powered by the
[FlashRaw](https://github.com/ybouane/flashraw) Rust decoder compiled to WebAssembly.

- **Metadata in milliseconds**: camera, lens, exposure, CFA, black/white levels, white balance,
  colour matrices, previews, with no pixel decoding.
- **Instant previews**: embedded JPEG previews/thumbnails as bytes (zero decoding), or as RGBA.
- **Sensor data**: `raw-u16`, `raw-f32`, `normalized`, `linear-rgb`, or display-ready `rgba8`
  (`ImageData`-compatible), bit-identical to the native FlashRaw decoder.
- **WASM SIMD128** binary with automatic scalar fallback; ~756 KiB brotli (all decoders, built-in colour spaces and ICC profiles).
- **JPEG XL DNGs** (DNG 1.7): the codec lives in separate, larger binaries that are downloaded
  only when a JPEG XL file is decoded; every other file uses the small build.
- **Multithreaded**: on cross-origin isolated pages and in Node.js a threaded build decodes one
  image on all cores (shared memory + Web Workers); single-threaded everywhere else, same API.
- **WebGPU** (opt-in build): GPU develop/unpack, results read back or left in a `GPUBuffer` /
  `GPUTexture` for your renderer.
- **Off the main thread**: built-in Web Worker / `worker_threads` pool (`flashraw/worker`).
- **TypeScript** types for everything; structured errors (`err.kind`).
- One package for Vite, webpack 5, Rollup/Rolldown, esbuild, Node.js ≥ 18, Deno.

Formats: DNG, CR2, CR3, CRW, NEF/NRW, ARW/SR2/SRF/ARQ, RAF, RW2/RWL, ORF/ORI, PEF, SRW, MRW,
IIQ, MOS, 3FR/FFF, KDC/DCR/DCS, ERF, MEF, X3F, ARI and more (2,008 of the 2,016 files of
the raw.pixls.us corpus decode; the rest are formats not supported yet, see `docs/formats.md`). The JS API is format-agnostic.

```sh
npm install flashraw
```

## Quick start

```js
import { FlashRaw } from 'flashraw';

const raw = await FlashRaw.open(buffer); // ArrayBuffer | Uint8Array | Node Buffer | Blob/File
raw.metadata.camera.name;                // "Canon EOS 5D Mark III"
raw.metadata.width; raw.metadata.height; // size after default crop

const preview = await raw.preview();     // { data: Uint8Array, mimeType, width, height, orientation } | null

const frame = await raw.decode({ backend: 'auto', output: 'raw-u16' });
frame.width; frame.height; frame.cfa?.pattern; frame.backend.decode; frame.timings; frame.data; // Uint16Array

const img = await raw.decode({ output: 'rgba8' });
ctx.putImageData(new ImageData(img.data, img.width, img.height), 0, 0);

raw.close(); // or: using raw = await FlashRaw.open(buffer);
```

## Browser

```html
<input type="file" id="pick">
<canvas id="c"></canvas>
<script type="module">
  import { FlashRaw } from 'flashraw';

  document.getElementById('pick').onchange = async (e) => {
    const file = e.target.files[0];
    const raw = await FlashRaw.open(file); // the File is streamed into WASM memory (one copy)
    try {
      // 1. instant: the embedded JPEG, decoded natively by the browser
      const p = await raw.preview();
      if (p?.mimeType === 'image/jpeg') {
        document.body.append(Object.assign(new Image(), { src: URL.createObjectURL(new Blob([p.data], { type: p.mimeType })) }));
      }
      // 2. the real RAW decode
      const img = await raw.decode({ output: 'rgba8', demosaic: 'half-size' });
      const c = document.getElementById('c');
      c.width = img.width;
      c.height = img.height;
      c.getContext('2d').putImageData(img.toImageData(), 0, 0);
    } finally {
      raw.close();
    }
  };
</script>
```

The `.wasm` files are referenced as `new URL('./flashraw_simd.wasm', import.meta.url)`, which
Vite, webpack 5, Rollup/Rolldown and Parcel turn into emitted assets automatically. Serve them as
`application/wasm` for streaming compilation.

## Node.js

```js
import { FlashRaw, readMetadata, extractPreview, decodeRaw } from 'flashraw';
import { readFile, writeFile } from 'node:fs/promises';

const meta = await readMetadata('photo.dng');                 // paths work everywhere in Node
const raw = await FlashRaw.openFile('photo.dng');              // read in chunks straight into WASM memory
const fromBuffer = await FlashRaw.open(await readFile('x.dng')); // Buffer / Uint8Array / ArrayBuffer

const p = await extractPreview('photo.dng');
if (p) await writeFile('preview.jpg', p.data);

const frame = await decodeRaw('photo.dng', { output: 'raw-u16' });
console.log(frame.width, frame.height, frame.cfa?.pattern, frame.timings.totalMs);
```

Node uses the same WebAssembly module as browsers (no native addon to compile or download).
Set `FLASHRAW_WASM_SIMD=0` to force the scalar binary. See
[docs/wasm.md](../docs/wasm.md#node-wasm-instead-of-a-native-addon) for the rationale and numbers.

## React + Vite

```jsx
import { useEffect, useState } from 'react';
import { createWorkerPool } from 'flashraw/worker';

const pool = createWorkerPool({ size: 2 });

export function useRaw(file) {
  const [state, setState] = useState(null);
  useEffect(() => {
    if (!file) return;
    const ac = new AbortController(); // a new file (or unmount) cancels the pending work
    let raw;
    (async () => {
      raw = await pool.open(file, { signal: ac.signal });
      const frame = await raw.decode({ output: 'rgba8', demosaic: 'half-size', signal: ac.signal });
      setState({ metadata: raw.metadata, frame });
    })()
      .catch((e) => ac.signal.aborted || console.error(e))
      .finally(() => ac.signal.aborted && raw?.close());
    return () => {
      ac.abort();
      raw?.close();
    };
  }, [file]);
  return state;
}
```

```js
// vite.config.js: keep flashraw out of dev pre-bundling so its .wasm URLs resolve
export default { optimizeDeps: { exclude: ['flashraw'] }, worker: { format: 'es' } };
```

A complete app is in [`examples/react-vite`](examples/react-vite).

## Web Worker (decode off the main thread)

```js
import { createWorkerPool } from 'flashraw/worker';

const pool = createWorkerPool({ size: 4 });             // workers spawn lazily
const frame = await pool.decode(file, { output: 'rgba8' }); // File/Blob read inside the worker
const meta = await pool.readMetadata(arrayBuffer, { transfer: true }); // move, don't copy

// keep a file open in one worker for repeated calls
const raw = await pool.open(file);
raw.metadata;                         // returned eagerly
const prev = await raw.preview();
const tile = await raw.decodeTile(0);
await raw.close();

pool.terminate();
```

Requests queue in the pool and each worker runs one at a time (whichever worker is free picks
up the next request; calls on a remote handle wait for the worker holding it). Every method
takes a `signal` to cancel:

```js
const ac = new AbortController();
const frame = pool.decode(file, { output: 'rgba8', signal: ac.signal });
ac.abort(); // rejects with an AbortError (or the reason given to abort())
await pool.decode(file, { signal: AbortSignal.timeout(10_000) }); // bound the decode time
```

A request that has not started is dropped. A running one cannot be interrupted inside WASM, so
its worker is terminated (and replaced on demand); remote handles opened in that worker are
closed (`raw.closed`, later calls reject with `ClosedError`). `FlashRaw` on the calling thread
has no cancellation: use the pool when a decode must be stoppable.

In Node.js the same import gives a `worker_threads` pool (paths are opened by the workers).
Results arrive as transferred buffers (no copy). Each pool worker runs the single-threaded build
(pass `init: { threads: n }` to give each its own thread pool); a pool decodes several files in
parallel (≈2.7× with 4 workers on 8 files), while the multithreaded build (below) makes one
decode faster.

## Multithreading

`init()` picks the multithreaded build automatically when it can run:

- **Browsers**: the page must be *cross-origin isolated* (`self.crossOriginIsolated === true`),
  which is what enables `SharedArrayBuffer`. Serve the page with

  ```
  Cross-Origin-Opener-Policy: same-origin
  Cross-Origin-Embedder-Policy: require-corp     (or: credentialless)
  ```

  Every cross-origin subresource then needs CORS or `Cross-Origin-Resource-Policy`. Without
  isolation (or without WebAssembly SIMD) the single-threaded build is used, with the reason in
  `getCapabilities().threads.reason`; nothing else changes.
- **Node.js**: always (shared memory + `worker_threads`); `FLASHRAW_WASM_THREADS=0` disables it,
  `FLASHRAW_WASM_THREADS=8` sets the count.

```js
import { init, initThreads, getCapabilities } from 'flashraw';
await initThreads(8);              // optional, before any other call: thread count (default: hardware concurrency)
await init({ threads: false });    // or force the single-threaded build
(await getCapabilities()).threads; // { wasmThreads, count, reason, crossOriginIsolated, sharedArrayBuffer, ... }
```

Threads are Web Workers (`dist/thread-worker.browser.js`) sharing one `WebAssembly.Memory`: one
coordinator runs the decode, the others are rayon workers; the main thread never blocks, so
`await raw.decode()` keeps the UI responsive. Output is bit-identical to the single-threaded build.
The thread count is fixed at initialisation. `zeroCopy` views point into a `SharedArrayBuffer`
(`toImageData()` copies; the default copied output is an ordinary `ArrayBuffer`).

Dev servers: Vite `server: { headers: { 'Cross-Origin-Opener-Policy': 'same-origin',
'Cross-Origin-Embedder-Policy': 'require-corp' } }` (also `preview`); webpack-dev-server
`devServer: { headers: { ...same } }`. Both bundle the thread worker and the threaded `.wasm`
automatically (literal `new Worker(new URL(..., import.meta.url))`); with esbuild copy
`dist/thread-worker.browser.js`, `threads.js`, `errors.js`, `flashraw_bindgen_threads.js` and
`flashraw_simd_threads.wasm` next to your bundle, or pass `init({ threadWorkerUrl })`.

## Metadata only

```js
const m = await readMetadata(fileOrBytes);
m.format;                  // "DNG"
m.camera;                  // { name, make, model, canonicalMake, canonicalModel, uniqueModel, serial, firmware }
m.lens;                    // { make, model, focalRange, apertureRange, serial }
m.exposure;                // { iso, exposureTime, fNumber, focalLength, focalLength35mm, exposureBias, ... }
m.captureTime; m.gps; m.orientation;           // orientation to apply, EXIF code 1..8
m.orientationInfo;         // { exif, makerNote, autoRotate, resolved, source, policy }: the standard tag,
                           //   the vendor record (Canon CameraOrientation, Nikon edit rotation, Kodak camera
                           //   JPEG), the Auto Rotate setting, and which one `orientation` came from
m.width; m.height; m.rawWidth; m.rawHeight; m.bitsPerSample; m.compression;
m.cfa;                     // { pattern: "RGGB", width: 2, height: 2, colors: [...] } | null
m.blackLevel; m.whiteLevel; m.whiteBalance; m.colorMatrices;
m.whiteBalanceApplied;     // true when the camera already applied m.whiteBalance (Sony lossless YCbCr M/S,
                           //   Nikon sNEF): rgba8/RGB outputs develop with unit multipliers; don't apply it again
m.previews;                // [{ index, kind, encoding, mimeType, width, height, byteLength, offset, orientation }]
                           //   (each preview's orientation is resolved on its own: it can differ from m.orientation)
m.extra;                   // vendor extras { key: value }
m.dng;                     // DNG only (else null): { baselineExposure, baselineNoise, baselineSharpness,
                           //   linearResponseLimit, asShotNeutral, asShotWhiteXY, defaultUserCrop,
                           //   defaultScale, noiseProfile, profile: { name, embedPolicy, hueSatMapDims,
                           //   hueSatMapCount, lookTableDims, toneCurve, baselineExposureOffset } }
m.opcodes;                 // DNG opcodes: [{ list, id, name, optional, previewOnly, applied, params }]
                           //   params for WarpRectilinear ({ planes: [{ kr, kt }], center }) and
                           //   FixVignetteRadial ({ k, center }), so editors can apply them themselves
```

`metadata` is plain JSON (safe to `postMessage`, store or `JSON.stringify`). `raw.frameInfo`
adds the sensor-buffer geometry (active area, crop, masked areas), `raw.capabilities` tells
whether random access/tiles are available, `raw.tiles` lists decodable tiles.

## RAW decode

```js
const f = await raw.decode({
  output: 'raw-u16',        // 'raw-u16' | 'raw-f32' | 'normalized' | 'linear-rgb' | 'linear-rgb16'
                            // | 'linear-rgb-f16' | 'rgba8' | 'rgb16'
  backend: 'auto',          // 'auto' | 'cpu' (scalar reference) | 'cpu-simd' | 'gpu'
  demosaic: 'bilinear',     // RGB outputs: 'bilinear' | 'half-size'
  crop: true,               // RGB outputs: apply the default crop; raw outputs: 'active' = active area only
  sampleType: 'f32',        // normalized, linear-rgb: 'f16' = half floats (Uint16Array bits, half the memory)
  exposure: 0,              // RGB outputs: EV applied to linear values before encoding
  blackPoint: 0,            // RGB outputs: linear value mapped to black (after exposure)
  whitePoint: 1,            // RGB outputs: linear value mapped to white (integer outputs clip)
  corruptionPolicy: 'recover', // truncated sensor data: 'recover' (zero-fill + warnings) | 'strict' (throw)
  previewFallback: false,   // RGB outputs of undecodable sensor data: true = embedded full-size preview
  colorSpace: 'srgb',       // RGB outputs: 'srgb' | 'display-p3' | 'adobe-rgb' | 'prophoto' | 'rec2020'
  outputProfile: undefined, // RGB outputs: Uint8Array of an ICC profile to convert to (instead of colorSpace)
  intent: 'perceptual',     // for outputProfile: 'perceptual' | 'relative' | 'saturation' | 'absolute'
  blackPointCompensation: false, // for outputProfile (profiles with lookup tables)
  limits: { maxSamples: 4e8 },
});
f.data;            // Uint16Array (raw-u16, rgb16, linear-rgb16, linear-rgb-f16, f16 sample types)
                   // | Float32Array (raw-f32, normalized, linear-rgb) | Uint8ClampedArray (rgba8)
f.dataType;        // 'u8' | 'u16' | 'f16' | 'f32'
f.channels;        // 1 (CFA), 3 (linear RGB outputs, rgb16, linear DNG), 4 (rgba8)
f.orientation;     // still to apply: 1 for rgba8/rgb16 (already oriented); linear outputs keep the sensor's
f.cfa; f.blackLevel; f.whiteLevel; f.whiteBalance; f.colorMatrices; f.activeArea; f.crop; f.origin;
f.whiteBalanceApplied;  // see m.whiteBalanceApplied
f.backend;         // { requested, kind: 'cpu', decode: 'CPU WASM SIMD128 x1 thread', simd, fallbackReason }
f.timings;         // { totalMs, stages: { containerParse, entropyDecode, postprocess, ... }, copyMs, wallMs }
f.incomplete;      // true when some samples hold no data (truncated/corrupt input recovered)
f.warnings;        // [{ kind: 'truncated', unit: 'strip', index, offset, expected, available, fileLength, samplesFilled, message }]
f.source;          // 'sensor' | 'preview' (previewFallback substituted the embedded preview)
f.colorSpace;      // 'srgb' | 'display-p3' | ... | 'custom' (outputProfile); 'srgb' for raw outputs
f.iccProfile;      // Uint8Array: ICC profile of the RGB samples, for tagging files (null for raw outputs)

// For an editor that demosaics on its own GPU: black-subtracted, white-normalised CFA (DNG
// opcode lists 1 and 2 applied) as half floats, cropped to the active area. `f.origin` is the
// crop's position in the sensor buffer and `f.cfa.pattern` is anchored at it; upload
// `f.data` straight into an `r16float` texture.
using cfa = await raw.decode({ output: 'normalized', sampleType: 'f16', crop: 'active', zeroCopy: true });

const region = await raw.decodeRegion({ x: 0, y: 0, width: 512, height: 512 });
const tile = await raw.decodeTile(3); // see raw.tiles
```

**Truncated files.** Sensor data that runs past the end of the file is never silently
zero-filled. By default (`corruptionPolicy: 'recover'`) every complete sample is decoded, the
missing ones are zero, and `f.incomplete` / `f.warnings` say which strip or tile lost how many
bytes and samples. With `corruptionPolicy: 'strict'` the decode throws a `CorruptFileError`
whose `corrupt` field holds the same details (`{ kind: 'truncated', unit, index, offset,
expected, available, fileLength }`). Metadata, previews and regions that do not touch the
missing data are unaffected. Recovery covers uncompressed/packed data and compressed DNG
strips/tiles (a unit that is truncated or fails to decode is zero-filled with an
`invalid-unit` or `truncated` warning) and Nikon/Pentax Huffman and Samsung SRW streams cut
short by the end of the file (the rows decoded before the cut are kept); other compressed
streams throw under both policies.

**Undecodable sensor data.** Files whose sensor data uses a codec FlashRaw cannot
decode throw an `UnsupportedCompressionError` by default, even for `rgba8`.
With `previewFallback: true`, RGB outputs (`rgba8`, `linear-rgb`) return the embedded
full-size preview instead, explicitly marked: `f.source === 'preview'` and a warning of kind
`preview-fallback` (so `f.incomplete` is true). Such a frame is the camera's rendering (8-bit,
display-referred), not sensor data; raw outputs and regions never fall back.

**Colour management.** RGB outputs are linear sRGB / sRGB-encoded by default. `colorSpace`
(`'display-p3'`, `'adobe-rgb'`, `'prophoto'`, `'rec2020'`) develops straight to another space
at no extra per-pixel cost: the linear outputs get its primaries, `rgba8` / `rgb16` also its
transfer curve, and `f.iccProfile` is the matching standard ICC profile (a `Uint8Array` to embed
in a PNG/JPEG/TIFF, or to tag a canvas pipeline). `outputProfile` converts to any RGB ICC
profile instead (the bytes of a `.icc` file; `intent`, `blackPointCompensation`); matrix/TRC
profiles are folded into the develop pass, profiles with lookup tables take a slower second
pass and only the display outputs. Invalid profiles reject with `InvalidArgumentError`. The GPU
backend handles the built-in spaces; `outputProfile` decodes use the CPU. See
[docs/color-management.md](../docs/color-management.md).

**Memory.** By default `f.data` is a standalone copy (the WASM buffer is freed immediately), so it
can be kept, transferred to a worker, or wrapped in `ImageData`. For large frames you can skip
that copy:

```js
using f = await raw.decode({ zeroCopy: true }); // f.data is a view into WASM memory
process(f.data);                                 // re-read f.data after other flashraw calls
// freed at the end of the block (or call f.free(); f.detach() copies out and frees)
```

## Preview extraction

```js
const p = await raw.preview();      // largest embedded image, or null
const t = await raw.thumbnail();    // smallest, or null
const nth = await raw.preview({ index: 1 });
const rgba = await raw.preview({ format: 'rgba8' }); // decoded (JPEG/RGB previews) → Uint8ClampedArray
const view = await raw.preview({ zeroCopy: true });  // view into the input buffer, valid until close()
const jpeg = await extractPreview(fileOrBytes);      // one-shot
```

`p.data` is the stored bytes (usually JPEG; `mimeType` tells), so browsers can display it with
`createImageBitmap(new Blob([p.data], { type: p.mimeType }))` at native speed.

## GPU decoding

The WebGPU backend ships as a separate, opt-in binary (`flashraw_simd_gpu.wasm`, +55 KiB brotli
and +52 KiB of JS bindings; single-threaded). Load it before any other call:

```js
await init({ gpu: true });

// Read back into a typed array (same shape as the CPU result):
const f = await raw.decode({ backend: 'gpu', output: 'rgba8' });   // fallback: 'cpu' by default
f.backend.kind;            // 'gpu' when WebGPU ran, otherwise 'cpu'
f.backend.fallbackReason;  // why not (no WebGPU, no adapter, binary without GPU, ...)
await raw.decode({ backend: 'gpu', fallback: 'error' });            // throws GpuUnavailableError instead

// Keep the result on the GPU: 'gpu-rgba8' | 'gpu-raw-u16' | 'gpu-normalized' | 'gpu-linear-rgb'
const g = await raw.decode({ output: 'gpu-rgba8' });
if (g instanceof GpuFrame) {
  g.buffer;                // GPUBuffer (STORAGE | COPY_SRC), one RGBA8 pixel per u32; g.byteLength bytes
  const tex = g.createTexture(); // GPUTexture rgba8unorm (TEXTURE_BINDING | STORAGE_BINDING | COPY_SRC), yours
  const host = await g.download(); // a regular DecodedFrame<'rgba8'>
  g.free();                // returns the buffer to FlashRaw's pool (or: const buf = g.detachBuffer())
}

const device = await getGpuDevice(); // the GPUDevice FlashRaw uses (null without WebGPU)
(await getCapabilities()).gpu;       // { compiled, webgpu, available, reason, adapter }
```

**Sharing the device.** GPU outputs live on FlashRaw's own `GPUDevice`: build your renderer on
`getGpuDevice()` to bind `buffer`/`createTexture()` directly (zero copy). Passing in an
application-created `GPUDevice` is not supported (the underlying wgpu WebGPU backend cannot wrap
a foreign device). With another device, `download()` and upload, or use `rgba8` output.

Without WebGPU (no `navigator.gpu`, no adapter, Node.js) `gpu-*` outputs fall back to the host
output (`gpu-rgba8` → a CPU `rgba8` `DecodedFrame`) unless `fallback: 'error'`. Raw outputs of
entropy-coded files decode on the CPU (that *is* the work); bit-packed files are unpacked on the
GPU straight from the file bytes. GPU `rgba8` matches the CPU result within ±2 per channel.

## JPEG XL DNGs

DNG 1.7 allows JPEG XL compression (tag 52546; recent phones and DNG converters write it). The
JPEG XL decoder adds ~55 % to the binary, so it ships as separate variants of the SIMD builds:
`flashraw_simd_jxl.wasm` and `flashraw_simd_threads_jxl.wasm`. By default nothing changes for
other files:

```js
await init();                        // jpegxl: 'auto' (default): the small build
const raw = await FlashRaw.open(file); // metadata, previews: small build, even for JPEG XL DNGs
raw.frameInfo.compression;           // 'jpeg-xl'
const f = await raw.decode();        // first JPEG XL decode: loads the JPEG XL variant (once), then decodes
f.backend.binary;                    // 'simd-jxl' / 'threads-jxl' (or 'simd' / 'threads' for other files)
raw.binary;                          // the binary now serving this file

await init({ jpegxl: 'always' });    // load the JPEG XL variant up front, instead of the small build
await init({ jpegxl: 'never' });     // never load it: decoding a JPEG XL DNG throws UnsupportedCompressionError
await loadJpegXlSupport();           // 'auto' mode: preload it now (e.g. when the user picks a .dng)
(await getCapabilities()).jpegxl;    // { mode, compiled, loaded, binary, available, reason }
```

- `'auto'` detects JPEG XL from the frame's compression when the file is decoded (opening,
  metadata and previews never need the codec) and moves that file to the JPEG XL variant, which
  matches the selected build: threaded (`flashraw_simd_threads_jxl.wasm`, with its own thread
  pool of the same size) on cross-origin isolated pages and in Node.js, single-threaded
  otherwise. It is loaded once and shared; other files keep using the small build. A load
  failure surfaces as `UnsupportedCompressionError` with the reason (and is retried on the next
  JPEG XL decode).
- Works the same in the `flashraw/worker` pool (each worker loads it on demand;
  `createWorkerPool({ init: { jpegxl: 'never' } })` to opt out) and in Node.js
  (`FLASHRAW_WASM_JPEGXL=auto|always|never`).
- Requires WebAssembly SIMD: the scalar fallback build has no JPEG XL variant. The WebGPU build
  has none either: with `init({ gpu: true })` JPEG XL files are decoded by `flashraw_simd_jxl.wasm`
  on the CPU (GPU outputs fall back with `backend.fallbackReason`).
- Self-hosting: `init({ baseUrl })` covers the JPEG XL files too; `init({ jpegxlWasm })` passes
  the module explicitly (like `wasm`). The package also exports them as `flashraw/wasm/simd-jxl`
  and `flashraw/wasm/threads-jxl`.
- Output: lossless JPEG XL decodes bit-identically to the native FlashRaw build; lossy JPEG XL is
  floating-point, and native x86-64 runs the codec's AVX2/FMA kernels, so a small fraction of
  samples (~0.1 % in testing) can round one level apart from the WASM result.

## Binaries and sizes

| Binary | When | Raw | gzip | brotli |
|---|---|---|---|---|
| `flashraw_simd.wasm` | default (no cross-origin isolation) | 2897 KiB | 1006 KiB | 756 KiB |
| `flashraw_simd_threads.wasm` | default when threads are available | 2976 KiB | 998 KiB | 745 KiB |
| `flashraw.wasm` | engines without WebAssembly SIMD | 2847 KiB | 986 KiB | 738 KiB |
| `flashraw_simd_gpu.wasm` | `init({ gpu: true })` | 3087 KiB | 1082 KiB | 812 KiB |
| `flashraw_simd_jxl.wasm` | first JPEG XL DNG (single-threaded), or `jpegxl: 'always'` | 4362 KiB | 1501 KiB | 1088 KiB |
| `flashraw_simd_threads_jxl.wasm` | first JPEG XL DNG (threaded), or `jpegxl: 'always'` | 4434 KiB | 1488 KiB | 1078 KiB |

Plus 24–76 KiB of generated JS glue per binary (4–13 KiB gzip). A page downloads one binary,
plus one JPEG XL variant only if it decodes a JPEG XL DNG.

## CPU fallback and SIMD

`init()` runs implicitly on first use. It loads `flashraw_simd_threads.wasm` when threads are
available (see Multithreading), else `flashraw_simd.wasm` on engines with WebAssembly SIMD
(Chrome/Edge ≥ 91, Firefox ≥ 89, Safari ≥ 16.4, Node ≥ 16.4) and `flashraw.wasm` otherwise, or
`flashraw_simd_gpu.wasm` with `init({ gpu: true })`. Only one binary is downloaded (plus a
JPEG XL variant for JPEG XL DNGs, see above); all produce identical CPU output. `getCapabilities().wasm.binary` says which one is in use.

```js
import { init, getCapabilities } from 'flashraw';
await init({ simd: false });                   // force the scalar binary
await init({ baseUrl: 'https://cdn.example/flashraw/' }); // host the .wasm files yourself
await init({ wasm: await fetch('/flashraw_simd.wasm') });  // or pass the module/bytes/Response
(await getCapabilities()).simd;                // { supported, active, level, fallbackReason }
```

## Errors

Everything throws a `FlashRawError` subclass with a stable `kind`:

```js
import { FlashRawError, CorruptFileError, UnsupportedFormatError } from 'flashraw';
try {
  await FlashRaw.open(bytes);
} catch (e) {
  if (e instanceof UnsupportedFormatError) { /* not a RAW file */ }
  else if (e instanceof FlashRawError) console.log(e.kind, e.message); // 'CorruptFile', 'LimitExceeded', ...
}
```

Kinds: `UnsupportedFormat`, `UnsupportedCompression`, `CorruptFile`, `InvalidMetadata`,
`CameraUnsupported`, `GpuUnavailable`, `OutOfMemory`, `LimitExceeded`, `NotAvailable`,
`InvalidArgument`, `Io`, `Closed`, `Internal` (a WASM trap; the handle becomes unusable).
Errors survive worker boundaries with the same classes. `CorruptFile` errors about truncated
sensor data also carry `e.corrupt` (see *Truncated files* above).

## Bundlers

| Bundler | Setup |
|---|---|
| Vite / Rollup / Rolldown | none (dev server: `optimizeDeps: { exclude: ['flashraw'] }`; COOP/COEP `server.headers` for threads) |
| webpack 5 | none (`new URL(..., import.meta.url)` assets and workers are built in; COOP/COEP `devServer.headers` for threads) |
| esbuild | copy `node_modules/flashraw/dist/*.wasm` next to your bundle (or `init({ baseUrl })`); use `createWorkerPool({ workerUrl })` for workers and `init({ threadWorkerUrl })` for threads |
| Node / Deno | none |

All six `.wasm` files are emitted as assets; a page downloads only the one `init()` selects, and
a JPEG XL variant (with its glue, a separate chunk) only when it decodes a JPEG XL DNG.

## TypeScript

Types ship with the package (`dist/index.d.ts`, `dist/worker.d.ts`); output types follow the
`output` option (`decode({ output: 'rgba8' })` → `DecodedFrame<'rgba8'>` whose `data` is a
`Uint8ClampedArray<ArrayBuffer>`). TypeScript ≥ 5.7 is recommended.

## License

MIT OR Apache-2.0.

<!-- flashraw-outro -->
## Built with FlashRaw

- **[LightFlow](https://lightflow.dev)**: professional photo correction and colour grading, running locally in your browser with WebGPU.
- **[ImageFlow](https://imageflow.dev)**: a Photoshop-class image editor in your browser.

Official website: **[flashraw.ybouane.com](https://flashraw.ybouane.com)** · Created by [@ybouane](https://x.com/ybouane) · Licence: MIT OR Apache-2.0
<!-- /flashraw-outro -->
