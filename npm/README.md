# flashraw

Fast, safe camera RAW decoding for browsers and Node.js, powered by the
[FlashRaw](https://github.com/ybouane/flashraw) Rust decoder compiled to WebAssembly.

- **Metadata in milliseconds**: camera, lens, exposure, CFA, black/white levels, white balance,
  colour matrices, previews, with no pixel decoding.
- **Instant previews**: embedded JPEG previews/thumbnails as bytes (zero decoding), or as RGBA.
- **Sensor data**: `raw-u16`, `raw-f32`, `normalized`, `linear-rgb`, or display-ready `rgba8`
  (`ImageData`-compatible), bit-identical to the native FlashRaw decoder.
- **WASM SIMD128** binary with automatic scalar fallback; ~215 KiB brotli.
- **Off the main thread**: built-in Web Worker / `worker_threads` pool (`flashraw/worker`).
- **TypeScript** types for everything; structured errors (`err.kind`).
- One package for Vite, webpack 5, Rollup/Rolldown, esbuild, Node.js ≥ 18, Deno.

Formats: DNG today, with CR2/CR3/NEF/ARW/RAF/RW2/ORF/PEF/… arriving as the Rust decoders land.
The JS API is format-agnostic.

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
    let raw;
    let cancelled = false;
    (async () => {
      raw = await pool.open(file);
      const frame = await raw.decode({ output: 'rgba8', demosaic: 'half-size' });
      if (!cancelled) setState({ metadata: raw.metadata, frame });
    })();
    return () => {
      cancelled = true;
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

In Node.js the same import gives a `worker_threads` pool (paths are opened by the workers).
Results arrive as transferred buffers (no copy). The WASM module itself is single-threaded;
a pool decodes several files in parallel (≈2.7× with 4 workers on 8 files; see docs/wasm.md).

## Metadata only

```js
const m = await readMetadata(fileOrBytes);
m.format;                  // "DNG"
m.camera;                  // { name, make, model, canonicalMake, canonicalModel, uniqueModel, serial, firmware }
m.lens;                    // { make, model, focalRange, apertureRange, serial }
m.exposure;                // { iso, exposureTime, fNumber, focalLength, focalLength35mm, exposureBias, ... }
m.captureTime; m.gps; m.orientation;           // EXIF orientation 1..8
m.width; m.height; m.rawWidth; m.rawHeight; m.bitsPerSample; m.compression;
m.cfa;                     // { pattern: "RGGB", width: 2, height: 2, colors: [...] } | null
m.blackLevel; m.whiteLevel; m.whiteBalance; m.colorMatrices;
m.previews;                // [{ index, kind, encoding, mimeType, width, height, byteLength, offset, orientation }]
m.extra;                   // vendor extras { key: value }
```

`metadata` is plain JSON (safe to `postMessage`, store or `JSON.stringify`). `raw.frameInfo`
adds the sensor-buffer geometry (active area, crop, masked areas), `raw.capabilities` tells
whether random access/tiles are available, `raw.tiles` lists decodable tiles.

## RAW decode

```js
const f = await raw.decode({
  output: 'raw-u16',        // 'raw-u16' | 'raw-f32' | 'normalized' | 'linear-rgb' | 'rgba8'
  backend: 'auto',          // 'auto' | 'cpu' (scalar reference) | 'cpu-simd' | 'gpu'
  demosaic: 'bilinear',     // RGB outputs: 'bilinear' | 'half-size'
  crop: true,               // RGB outputs: apply the default crop
  limits: { maxSamples: 4e8 },
});
f.data;            // Uint16Array (raw-u16) | Float32Array (raw-f32, normalized, linear-rgb) | Uint8ClampedArray (rgba8)
f.channels;        // 1 (CFA), 3 (linear-rgb / linear DNG), 4 (rgba8)
f.cfa; f.blackLevel; f.whiteLevel; f.whiteBalance; f.colorMatrices; f.activeArea; f.crop; f.origin;
f.backend;         // { requested, kind: 'cpu', decode: 'CPU WASM SIMD128 x1 thread', simd, fallbackReason }
f.timings;         // { totalMs, stages: { containerParse, entropyDecode, postprocess, ... }, copyMs, wallMs }

const region = await raw.decodeRegion({ x: 0, y: 0, width: 512, height: 512 });
const tile = await raw.decodeTile(3); // see raw.tiles
```

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

```js
const f = await raw.decode({ backend: 'gpu', fallback: 'cpu' });   // default fallback
f.backend.kind;            // 'gpu' when the WebGPU backend ran, otherwise 'cpu'
f.backend.fallbackReason;  // e.g. "this FlashRaw WASM binary was built without the `gpu` feature ..."

await raw.decode({ backend: 'gpu', fallback: 'error' });            // throws GpuUnavailableError instead

const caps = await getCapabilities();
caps.gpu;                  // { compiled, webgpu, available, reason }
```

The WebGPU backend is under development in the Rust crate; this package's published binaries do
not include it yet, so `backend: 'gpu'` currently always takes the fallback path with an explicit
reason. The option, result fields and errors are stable, so code written today picks the GPU up
automatically once a `gpu`-enabled build ships.

## CPU fallback and SIMD

`init()` runs implicitly on first use. It loads `flashraw_simd.wasm` on engines with WebAssembly
SIMD (Chrome/Edge ≥ 91, Firefox ≥ 89, Safari ≥ 16.4, Node ≥ 16.4) and `flashraw.wasm` otherwise.
Both produce identical output.

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
Errors survive worker boundaries with the same classes.

## Bundlers

| Bundler | Setup |
|---|---|
| Vite / Rollup / Rolldown | none (dev server: `optimizeDeps: { exclude: ['flashraw'] }`) |
| webpack 5 | none (`new URL(..., import.meta.url)` assets and workers are built in) |
| esbuild | copy `node_modules/flashraw/dist/*.wasm` next to your bundle (or `init({ baseUrl })`); use `createWorkerPool({ workerUrl })` for workers |
| Node / Deno | none |

## TypeScript

Types ship with the package (`dist/index.d.ts`, `dist/worker.d.ts`); output types follow the
`output` option (`decode({ output: 'rgba8' })` → `DecodedFrame<'rgba8'>` whose `data` is a
`Uint8ClampedArray<ArrayBuffer>`). TypeScript ≥ 5.7 is recommended.

## License

MIT OR Apache-2.0.
