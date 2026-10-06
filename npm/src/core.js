// Platform-independent implementation of the flashraw JS API.
//
// `browser.js` and `node.js` register a platform (how to load the .wasm binaries, how to read
// file paths) and re-export everything from here.
//
// Memory model (see docs/wasm.md):
//  - Input bytes are copied exactly once, into a buffer allocated inside WASM memory
//    (`InputBuffer`); Blobs/Files and Node file paths are streamed into it chunk by chunk, so
//    no full-size intermediate JS copy exists. The decoder borrows from that buffer.
//  - Decoded pixels are produced inside WASM memory. By default they are copied once into a
//    standalone typed array and the WASM buffer is freed immediately (`frame.data` is then an
//    ordinary, transferable typed array). With `zeroCopy: true` the frame keeps the WASM buffer
//    and `frame.data` is a view of it, valid until `frame.free()`.
//  - Handles are freed deterministically by `close()`/`free()` (and `using` declarations via
//    `Symbol.dispose`); a FinalizationRegistry (from wasm-bindgen) frees anything left to GC.
//
// JPEG XL DNGs: the default binaries are built without the JPEG XL codec (it adds ~55 % to the
// binary). An "engine" is one instantiated binary (glue + exports + thread pool). Files are
// opened in the primary engine; when one turns out to need JPEG XL (its frame compression is
// `jpeg-xl`), the JPEG XL variant matching the primary build is loaded once, on first decode,
// and that file's bytes move to it. Every other file keeps using the primary engine.

import * as baseGlue from './flashraw_bindgen.js';
import { ThreadPool } from './threads.js';
import {
  FlashRawError,
  ClosedError,
  GpuUnavailableError,
  InternalError,
  InvalidArgumentError,
  IoError,
  NotAvailableError,
  UnsupportedCompressionError,
  errorFromKind,
} from './errors.js';

export * from './errors.js';

export const OUTPUTS = Object.freeze([
  'raw-u16',
  'raw-f32',
  'normalized',
  'linear-rgb',
  'linear-rgb16',
  'linear-rgb-f16',
  'rgba8',
  'rgb16',
]);
/** GPU-resident outputs (WebGPU build): the result stays in a `GPUBuffer`. */
export const GPU_OUTPUTS = Object.freeze(['gpu-raw-u16', 'gpu-normalized', 'gpu-linear-rgb', 'gpu-rgba8']);
export const BACKENDS = Object.freeze(['auto', 'cpu', 'cpu-simd', 'gpu']);

// (module (func (result v128) i32.const 0 i8x16.splat i8x16.popcnt)) — same probe as
// wasm-feature-detect: validates only on engines with fixed-width SIMD (Chrome 91, Firefox 89,
// Safari 16.4, Node 16.4).
const SIMD_PROBE = new Uint8Array([
  0, 97, 115, 109, 1, 0, 0, 0, 1, 5, 1, 96, 0, 1, 123, 3, 2, 1, 0, 10, 10, 1, 8, 0, 65, 0, 253, 15, 253, 98, 11,
]);

/** Whether this JS engine supports WebAssembly SIMD128. */
export function simdSupported() {
  try {
    return typeof WebAssembly === 'object' && WebAssembly.validate(SIMD_PROBE);
  } catch {
    return false;
  }
}

const dispose = Symbol.dispose ?? Symbol.for('Symbol.dispose');
const now = () => (typeof performance !== 'undefined' ? performance.now() : Date.now());

let platform = {
  name: 'unknown',
  load: () => {
    throw new InternalError('no flashraw platform registered (import "flashraw", not an internal module)');
  },
};
let wb = baseGlue; // wasm-bindgen glue of the loaded binary
let wasm = null; // wasm-bindgen exports (incl. `memory`)
let build = null; // { version, simd, simdLevel, gpu, gpuAvailable, threads, jpegxl, maxSamples, variant, initMs, ... }
let threads = null; // ThreadPool of the multithreaded build
let primary = null; // the engine above: { wb, wasm, pool, build }
let gpuState = null; // Promise<{ reason, adapter }> once a WebGPU device was requested
let initPromise = null;
let initOptions = {}; // options of the successful init() (reused to load the JPEG XL variant)
let jxl = null; // engine decoding JPEG XL files once loaded (`primary` when it has the codec)
let jxlPromise = null; // pending load of the JPEG XL engine
let jxlLoads = 0; // JPEG XL variants loaded on demand (tests: exactly once, only when needed)
let jxlError = null; // why the last on-demand JPEG XL load failed
const defaults = {};

export const JPEGXL_MODES = Object.freeze(['auto', 'always', 'never']);

/** @internal Called once by the platform entry point. */
export function _setPlatform(p) {
  platform = p;
}

/** @internal Defaults for the implicit `init()` (the worker pool disables nested threads). */
export function _setDefaults(d) {
  Object.assign(defaults, d);
}

/**
 * Load and instantiate the WebAssembly module. Called implicitly by every API; call it
 * explicitly to control which binary is used or to warm up early. Idempotent: options are only
 * honoured by the first call.
 */
export function init(options = {}) {
  if (!initPromise) {
    initOptions = options ?? {};
    initPromise = doInit(initOptions).catch((e) => {
      initPromise = null;
      throw e;
    });
  }
  return initPromise;
}

function jpegxlMode(o) {
  if (o === undefined || o === null) return 'auto';
  if (!JPEGXL_MODES.includes(o)) {
    throw new InvalidArgumentError(`\`jpegxl\` must be "auto", "always" or "never", got ${JSON.stringify(o)}`);
  }
  return o;
}

async function doInit(options) {
  const t0 = now();
  const jxlMode = jpegxlMode(options.jpegxl ?? platform.defaultJpegxl?.() ?? defaults.jpegxl);
  // 'always': load the JPEG XL flavour of the selected binary instead of the small one.
  const always = jxlMode === 'always';
  const engineSimd = simdSupported();
  const pref = options.simd ?? platform.defaultSimd?.() ?? 'auto';
  let variant = pref === false || !engineSimd ? 'fallback' : 'simd';
  const reason =
    pref === false ? 'disabled by option' : !engineSimd ? 'engine lacks WebAssembly SIMD' : null;

  // 1. WebGPU build (opt-in: it is single-threaded and larger).
  let gpuInitReason = null;
  if (options.gpu === true) {
    if (variant !== 'simd') gpuInitReason = `the WebGPU build requires WebAssembly SIMD (${reason})`;
    else {
      try {
        const { glue, exports } = await initGpuBinary(options);
        const info = finishInit(glue, exports, null, {
          variant,
          binary: 'gpu',
          simdSupported: engineSimd,
          fallbackReason: null,
          threadsReason: 'the WebGPU build is single-threaded',
          gpuReason: null,
          jpegxlMode: jxlMode,
          t0,
        });
        // The WebGPU build has no JPEG XL flavour: JPEG XL files use the SIMD JPEG XL variant.
        if (always) await loadJpegXl().catch(() => {});
        return info;
      } catch (e) {
        if (options.wasm) throw toError(e, 'failed to load the FlashRaw WebGPU WebAssembly module');
        gpuInitReason = `the WebGPU build failed to load: ${e?.message ?? e}`;
      }
    }
  }

  // 2. Multithreaded build (default wherever shared memory + workers are available).
  const tOpt = options.threads ?? platform.defaultThreads?.() ?? defaults.threads ?? 'auto';
  let threadsReason = null;
  if (options.gpu === true) threadsReason = 'the WebGPU build was requested';
  else if (tOpt === false || tOpt === 0) threadsReason = 'disabled by option';
  else if (variant !== 'simd') threadsReason = `the threaded build requires WebAssembly SIMD (${reason})`;
  else if (!platform.threadSupport || !platform.spawnThread) threadsReason = 'threads are not supported on this platform';
  else threadsReason = platform.threadSupport();
  if (!threadsReason) {
    const count = threadCount(tOpt === true || tOpt === 'auto' ? undefined : tOpt);
    try {
      const binary = always ? 'threads-jxl' : 'threads';
      const { glue, exports, pool } = await initThreaded(options, count, binary, options.wasm);
      return finishInit(glue, exports, pool, {
        variant,
        binary,
        simdSupported: engineSimd,
        fallbackReason: null,
        threadsReason: null,
        gpuReason: gpuInitReason,
        jpegxlMode: jxlMode,
        t0,
      });
    } catch (e) {
      if (options.wasm) throw toError(e, 'failed to start the FlashRaw threaded WebAssembly module');
      threadsReason = `the threaded build failed to start: ${e?.message ?? e}`;
    }
  }

  // 3. Single-threaded SIMD / scalar build (the scalar one has no JPEG XL flavour).
  let exports;
  let binary = variant === 'simd' && always ? 'simd-jxl' : variant;
  let glue = binary === 'simd-jxl' ? await platform.glue('simd-jxl') : baseGlue;
  try {
    const source = options.wasm ?? (await platform.load(binary, options));
    exports = await glue.default({ module_or_path: await source });
  } catch (e) {
    if (variant !== 'simd' || options.wasm) throw toError(e, 'failed to load the FlashRaw WebAssembly module');
    // A SIMD-capable probe but a failing SIMD binary (exotic engines, broken CDN): retry scalar.
    variant = binary = 'fallback';
    glue = baseGlue;
    exports = await baseGlue.default({ module_or_path: await platform.load(variant, options) }).catch((e2) => {
      throw toError(e2, 'failed to load the FlashRaw WebAssembly module');
    });
  }
  return finishInit(glue, exports, null, {
    variant,
    binary,
    simdSupported: engineSimd,
    fallbackReason: variant === 'fallback' ? (reason ?? 'SIMD binary failed to load') : null,
    threadsReason,
    gpuReason: gpuInitReason,
    jpegxlMode: jxlMode,
    t0,
  });
}

function finishInit(glue, exports, pool, extra) {
  const { t0, ...rest } = extra;
  wb = glue;
  wasm = exports;
  threads = pool;
  build = { ...JSON.parse(wb.buildInfo()), ...rest, initMs: now() - t0 };
  primary = { wb, wasm, pool, build };
  if (build.jpegxl) jxl = primary;
  build.jpegxlReason = build.jpegxl ? null : jpegxlUnavailable();
  return { ...build };
}

/** Why JPEG XL files cannot be decoded with the selected build, or null if they can. */
function jpegxlUnavailable() {
  if (build.jpegxl) return null;
  if (build.jpegxlMode === 'never') return 'disabled by option (jpegxl: "never")';
  if (!build.simd) {
    return `the JPEG XL variants require WebAssembly SIMD (${build.fallbackReason ?? 'scalar build selected'})`;
  }
  return null;
}

/**
 * The engine decoding JPEG XL files: the primary one if it has the codec, otherwise the JPEG XL
 * variant matching it (threaded if the primary is threaded), loaded once and shared by every
 * handle. A failed load is not cached (a later decode retries).
 */
function loadJpegXl() {
  if (jxl) return Promise.resolve(jxl);
  const why = jpegxlUnavailable();
  if (why) return Promise.reject(new NotAvailableError(why));
  jxlPromise ??= (async () => {
    const t0 = now();
    const o = initOptions;
    let threadsReason = build.threadsReason;
    if (threads && !threads.broken) {
      try {
        const { glue, exports, pool } = await initThreaded(o, threads.count, 'threads-jxl', o.jpegxlWasm);
        return engineOf(glue, exports, pool, { binary: 'threads-jxl', threadsReason: null, t0 });
      } catch (e) {
        if (o.jpegxlWasm) throw e;
        threadsReason = `the threaded JPEG XL build failed to start: ${e?.message ?? e}`;
      }
    }
    const glue = await platform.glue('simd-jxl');
    const exports = await glue.default({ module_or_path: await (o.jpegxlWasm ?? platform.load('simd-jxl', o)) });
    return engineOf(glue, exports, null, { binary: 'simd-jxl', threadsReason, t0 });
  })().then(
    (e) => {
      jxl = e;
      jxlLoads++;
      jxlError = null;
      return e;
    },
    (e) => {
      jxlPromise = null;
      jxlError = `the JPEG XL variant failed to load: ${e?.message ?? e}`;
      throw new NotAvailableError(jxlError, { cause: e });
    },
  );
  return jxlPromise;
}

function engineOf(glue, exports, pool, { t0, ...rest }) {
  const info = { ...JSON.parse(glue.buildInfo()), variant: 'simd', ...rest, initMs: now() - t0 };
  return { wb: glue, wasm: exports, pool, build: info };
}

/** Compile a module source (`Response`, bytes, `Module`, URL) without instantiating it. */
async function compileSource(src) {
  src = await src;
  if (src instanceof WebAssembly.Module) return src;
  if (typeof src === 'string' || src instanceof URL) src = await fetch(src);
  if (typeof Response === 'function' && src instanceof Response) {
    if (!src.ok) throw new Error(`failed to fetch the WebAssembly module: ${src.status} ${src.statusText}`);
    if (typeof WebAssembly.compileStreaming === 'function') {
      // Streaming compilation needs `application/wasm`; fall back to buffering otherwise.
      const copy = src.clone();
      try {
        return await WebAssembly.compileStreaming(src);
      } catch {
        return WebAssembly.compile(await copy.arrayBuffer());
      }
    }
    return WebAssembly.compile(await src.arrayBuffer());
  }
  return WebAssembly.compile(src);
}

/** Load a threaded binary and start its workers; throws when anything is missing. */
async function initThreaded(options, count, binary, source) {
  const glue = await platform.glue(binary);
  const module = await compileSource(source ?? platform.load(binary, options));
  const exports = await glue.default({ module_or_path: module });
  const pool = await ThreadPool.start({
    spawn: (name) => platform.spawnThread(name, options),
    module,
    memory: exports.memory,
    count,
  });
  return { glue, exports, pool };
}

/** Load the WebGPU binary (no WebGPU device is requested yet). */
async function initGpuBinary(options) {
  const glue = await platform.glue('gpu');
  const exports = await glue.default({ module_or_path: await (options.wasm ?? platform.load('gpu', options)) });
  return { glue, exports };
}

function threadCount(opt) {
  if (typeof opt === 'number') {
    if (!Number.isInteger(opt) || opt < 1) throw new InvalidArgumentError('`threads` must be a positive integer, true, false or "auto"');
    return Math.min(opt, 256);
  }
  const hc = Number(platform.hardwareConcurrency?.() ?? 4);
  return Math.max(1, Math.min(Number.isFinite(hc) ? hc : 4, 64));
}

/**
 * Initialise the multithreaded build with `count` threads (default: hardware concurrency).
 * Must be the first FlashRaw call to take effect (threads are chosen at `init()`); resolves to
 * the number of threads in use (0 = single-threaded, see `getCapabilities().threads.reason`).
 */
export async function initThreads(count) {
  const info = await init({ threads: count ?? true });
  return info.threadCount ?? 0;
}

function ensureInit() {
  if (!wasm) throw new InternalError('FlashRaw is not initialised; `await init()` first');
}

/** Typed-array view of `eng`'s WASM memory. */
function heap(eng, Ctor, ptr, len) {
  return new Ctor(eng.wasm.memory.buffer, ptr, len);
}

/** Convert anything thrown by wasm-bindgen / the WASM module into a FlashRawError. */
function toError(e, context, eng = primary) {
  if (e instanceof FlashRawError) return e;
  if (e && typeof e === 'object' && typeof e.kind === 'string') {
    return errorFromKind(e.kind, e.message, e.corrupt ? { corrupt: e.corrupt } : undefined);
  }
  if (typeof WebAssembly === 'object' && e instanceof WebAssembly.RuntimeError) {
    let panic = null;
    try {
      panic = (eng?.wb ?? wb).takeLastPanic() ?? null;
    } catch {}
    const hint = panic ? `panic: ${panic}` : `${e.message} (likely out of memory)`;
    return new InternalError(`FlashRaw WASM trap: ${hint}`, { cause: e });
  }
  const msg = e && typeof e === 'object' && 'message' in e ? e.message : String(e);
  return new InternalError(context ? `${context}: ${msg}` : msg, { cause: e });
}

function wcall(fn, eng = primary) {
  try {
    return fn();
  } catch (e) {
    throw toError(e, undefined, eng);
  }
}

// --- inputs ------------------------------------------------------------------------------------

const isBlob = (x) => typeof Blob !== 'undefined' && x instanceof Blob;
const tag = (x) => Object.prototype.toString.call(x);

/** Uint8Array view of a byte-like input (no copy), or null. */
function asBytes(input) {
  if (input instanceof Uint8Array) return input;
  if (ArrayBuffer.isView(input)) return new Uint8Array(input.buffer, input.byteOffset, input.byteLength);
  const t = tag(input);
  if (t === '[object ArrayBuffer]' || t === '[object SharedArrayBuffer]') return new Uint8Array(input);
  return null;
}

function copyIn(eng, bytes) {
  const buf = wcall(() => new eng.wb.InputBuffer(bytes.length), eng);
  heap(eng, Uint8Array, buf.ptr, bytes.length).set(bytes);
  return buf;
}

/** Write `chunks` into `buf` (one copy, no full-size JS buffer); they must total `size` bytes. */
async function fill(eng, buf, size, chunks) {
  let off = 0;
  for await (const c of chunks) {
    if (off + c.length > size) throw new IoError(`input is longer than its announced size (${size} bytes)`);
    // Re-derive the view each chunk: memory may have grown (and detached old views) meanwhile.
    heap(eng, Uint8Array, buf.ptr + off, c.length).set(c);
    off += c.length;
  }
  if (off !== size) throw new IoError(`input ended after ${off} of ${size} bytes`);
}

/**
 * Stream a Blob/File into WASM memory. Some engines fail to stream (or read in one piece) very
 * large Blobs that they read fine in slices (WebKit: fetched Blobs over ~100 MB reject with
 * `NotReadableError`), so a failed stream is retried slice by slice.
 */
async function blobIn(eng, blob) {
  const size = blob.size;
  const buf = wcall(() => new eng.wb.InputBuffer(size), eng);
  try {
    try {
      await fill(eng, buf, size, blobChunks(blob));
    } catch (e) {
      if (e instanceof FlashRawError) throw e;
      await fill(eng, buf, size, blobSlices(blob));
    }
  } catch (e) {
    buf.free();
    throw e instanceof FlashRawError ? e : new IoError(`failed to read input: ${e?.message ?? e}`, { cause: e });
  }
  return buf;
}

const isShared = (b) => tag(b) === '[object SharedArrayBuffer]';

/**
 * Read a file (Node path) into WASM memory chunk by chunk. With shared memory (threaded build)
 * the reads land in WASM memory directly: a shared buffer is never detached by growth, so the
 * view stays valid across the await. Otherwise each chunk goes through a staging buffer, as a
 * view into non-shared memory would be detached if memory grew during the read.
 */
async function fileIn(eng, src) {
  const { size } = src;
  let buf;
  try {
    buf = wcall(() => new eng.wb.InputBuffer(size), eng);
  } catch (e) {
    await src.close();
    throw e;
  }
  const chunk = Math.max(1, Math.min(size, src.chunkSize ?? 8 << 20));
  const direct = isShared(eng.wasm.memory.buffer);
  const staging = direct ? null : new Uint8Array(chunk);
  let off = 0;
  try {
    while (off < size) {
      const n = Math.min(chunk, size - off);
      const got = await src.read(direct ? heap(eng, Uint8Array, buf.ptr + off, n) : staging.subarray(0, n), off);
      if (got === 0) break;
      if (!direct) heap(eng, Uint8Array, buf.ptr + off, got).set(staging.subarray(0, got));
      off += got;
    }
    if (off !== size) throw new IoError(`input ended after ${off} of ${size} bytes`);
  } catch (e) {
    buf.free();
    throw e instanceof FlashRawError ? e : new IoError(`failed to read input: ${e?.message ?? e}`, { cause: e });
  } finally {
    await src.close();
  }
  return buf;
}

async function* blobChunks(blob) {
  const reader = blob.stream().getReader();
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) return;
      yield value;
    }
  } finally {
    reader.releaseLock();
  }
}

const BLOB_SLICE = 8 << 20;

async function* blobSlices(blob) {
  for (let off = 0; off < blob.size; off += BLOB_SLICE) {
    yield new Uint8Array(await blob.slice(off, off + BLOB_SLICE).arrayBuffer());
  }
}

async function inputBuffer(eng, input) {
  const bytes = asBytes(input);
  if (bytes) return copyIn(eng, bytes);
  if (isBlob(input)) return blobIn(eng, input);
  if (typeof input === 'string' || input instanceof URL) {
    if (!platform.fileSource) {
      throw new InvalidArgumentError('file paths are only supported in Node.js; pass bytes, a Blob or a File');
    }
    return fileIn(eng, await platform.fileSource(input));
  }
  throw new InvalidArgumentError(
    `unsupported input type ${tag(input)}; expected ArrayBuffer, Uint8Array/Buffer, another ArrayBufferView, Blob/File${platform.fileSource ? ' or a file path' : ''}`,
  );
}

function checkObject(o, what) {
  if (o === undefined || o === null) return {};
  if (typeof o !== 'object' || Array.isArray(o)) throw new InvalidArgumentError(`${what} must be an object`);
  return o;
}

// --- decoded frames -------------------------------------------------------------------------

// `f16`: IEEE half floats as raw bits (`sampleType: 'f16'` normalized output).
const ARRAYS = { u16: Uint16Array, f16: Uint16Array, f32: Float32Array, u8: Uint8Array };
// Zero-copy frames / pinned handles are strongly referenced until freed: a bare view does not
// keep its owner alive, so GC must never free WASM memory a view still points at.
const live = new Set();

/** A decoded frame. Plain data fields plus `data` (a typed array). */
export class DecodedFrame {
  #data;
  #view; // { eng, frame, Ctor, ptr, len } while zero-copy
  constructor(info, data, view) {
    Object.assign(this, info);
    this.#data = data;
    this.#view = view;
    if (view) live.add(this);
  }
  /** Pixel data. For zero-copy frames, re-read this after other FlashRaw calls (memory growth). */
  get data() {
    const v = this.#view;
    if (v) {
      if (!this.#data || this.#data.buffer !== v.eng.wasm.memory.buffer) this.#data = heap(v.eng, v.Ctor, v.ptr, v.len);
      return this.#data;
    }
    if (this.#data === null) throw new ClosedError('frame has been freed');
    return this.#data;
  }
  /** True while `data` is a view into WASM memory. */
  get zeroCopy() {
    return !!this.#view;
  }
  /** Copy of the pixels detached from WASM memory (always safe to keep or transfer). */
  detach() {
    const d = this.data;
    if (!this.#view) return d;
    const copy = d.slice();
    this.free();
    this.#data = copy;
    return copy;
  }
  /** Release the WASM buffer of a zero-copy frame (no-op for copied frames). */
  free() {
    const v = this.#view;
    if (v) {
      this.#view = null;
      this.#data = null;
      live.delete(this);
      try {
        v.frame.free();
      } catch {}
    }
  }
  [dispose]() {
    this.free();
  }
  /** `ImageData` for `rgba8` frames (browsers / environments with ImageData). */
  toImageData() {
    if (this.output !== 'rgba8') throw new InvalidArgumentError('`toImageData()` requires output "rgba8"');
    if (typeof ImageData === 'undefined') throw new NotAvailableError('ImageData is not available in this environment');
    const d = this.data;
    // ImageData rejects views of shared memory (zero-copy frames of the threaded build): copy.
    const shared = Object.prototype.toString.call(d.buffer) === '[object SharedArrayBuffer]';
    return new ImageData(shared ? new Uint8ClampedArray(d) : d, this.width, this.height);
  }
  /** Plain, structured-clone friendly object (`data` included). */
  toJSON() {
    return { ...this, data: undefined };
  }
}

function makeFrame(eng, frame, extra) {
  const t0 = now();
  const info = JSON.parse(frame.infoJson());
  const kind = frame.kind;
  const Ctor = kind === 'u8' && info.output === 'rgba8' ? Uint8ClampedArray : ARRAYS[kind];
  const ptr = frame.ptr;
  const len = frame.len;
  info.dataType = kind;
  info.backend = {
    ...info.backend,
    requested: extra.requested,
    fallbackReason: extra.fallbackReason ?? null,
    // Which WASM binary decoded it ('simd', 'threads', 'simd-jxl', 'threads-jxl', ...).
    binary: eng.build.binary,
    jpegxl: !!eng.build.jpegxl,
  };
  info.timings = { ...info.timings, copyMs: 0, wallMs: 0 };
  let out;
  if (extra.zeroCopy) {
    out = new DecodedFrame(info, null, { eng, frame, Ctor, ptr, len });
  } else {
    const data = heap(eng, Ctor, ptr, len).slice();
    frame.free();
    info.timings.copyMs = now() - t0;
    out = new DecodedFrame(info, data, null);
  }
  out.timings.wallMs = now() - extra.t0;
  return out;
}

/** Why the GPU cannot be used without requesting a device, or null if it might be. */
function gpuStaticReason() {
  if (!build?.gpu) {
    return (
      'this FlashRaw WASM binary was built without the `gpu` feature (WebGPU backend not compiled in); ' +
      `load the WebGPU build with init({ gpu: true })${build?.gpuReason ? ` (${build.gpuReason})` : ''}`
    );
  }
  if (typeof navigator === 'undefined' || !navigator.gpu) {
    return 'WebGPU is not available in this environment (navigator.gpu is undefined)';
  }
  return null;
}

/** Request the WebGPU adapter/device once (WebGPU build). Resolves to `{ reason, adapter }`. */
function gpuStatus() {
  const why = gpuStaticReason();
  if (why) return Promise.resolve({ reason: why, adapter: null });
  gpuState ??= wb.gpuInit().then(
    (json) => ({ reason: null, adapter: JSON.parse(json) }),
    (e) => ({ reason: String(e?.message ?? e), adapter: null }),
  );
  return gpuState;
}

const isGpu = (b) => typeof b === 'string' && ['gpu', 'webgpu'].includes(b.toLowerCase());
const isGpuOutput = (o) => typeof o === 'string' && o.toLowerCase().startsWith('gpu-');

/**
 * A decoded image that stays on the GPU (`output: 'gpu-*'`, WebGPU build). Plain frame fields
 * plus WebGPU handles on FlashRaw's device (`getGpuDevice()`): `buffer` (a `GPUBuffer`, usage
 * `STORAGE | COPY_SRC`), `createTexture()` for `gpu-rgba8`, `download()` to read it back.
 * The buffer is recycled by FlashRaw after `free()`; `detachBuffer()` hands it over for good.
 */
export class GpuFrame {
  #out;
  constructor(info, out) {
    Object.assign(this, info);
    this.#out = out;
    live.add(this);
  }
  #get() {
    if (!this.#out) throw new ClosedError('GPU frame has been freed or its buffer detached');
    return this.#out;
  }
  /** Always true: pixels live in `buffer`, not in host memory. */
  get gpuResident() {
    return true;
  }
  /** The `GPUBuffer` with the pixels (it may be larger than `byteLength`). */
  get buffer() {
    return wcall(() => this.#get().buffer);
  }
  /** Bytes of pixel data at the start of `buffer`. */
  get byteLength() {
    return wcall(() => this.#get().byteLength);
  }
  /** `'u16'` (two samples per 32-bit word), `'f32'` or `'rgba8'` (one RGBA pixel per word). */
  get format() {
    return wcall(() => this.#get().format);
  }
  /** The `GPUDevice` owning `buffer`. */
  get device() {
    return wb.gpuDevice();
  }
  /** Copy a `gpu-rgba8` frame into a new `rgba8unorm` `GPUTexture` (TEXTURE_BINDING | STORAGE_BINDING | COPY_SRC); the caller owns it. */
  createTexture() {
    return wcall(() => this.#get().createTexture());
  }
  /** Read back into host memory: a regular `DecodedFrame` with the host output (`rgba8`, ...). */
  async download() {
    const t0 = now();
    const out = this.#get();
    let f;
    try {
      f = await out.download();
    } catch (e) {
      throw toError(e);
    }
    return makeFrame(primary, f, { requested: this.backend.requested, fallbackReason: null, zeroCopy: false, t0 });
  }
  /** Take ownership of `buffer` (FlashRaw will not recycle it; call `destroy()` when done). */
  detachBuffer() {
    const out = this.#get();
    const b = wcall(() => out.detachBuffer());
    this.free();
    return b;
  }
  /** Return the buffer to FlashRaw's pool (idempotent). */
  free() {
    const out = this.#out;
    this.#out = null;
    live.delete(this);
    if (out) {
      try {
        out.free();
      } catch {}
    }
  }
  [dispose]() {
    this.free();
  }
  toJSON() {
    return { ...this };
  }
}

/** The WebGPU device FlashRaw renders into (WebGPU build), or null when WebGPU is unavailable. */
export async function getGpuDevice() {
  await init();
  const s = await gpuStatus();
  return s.reason ? null : wb.gpuDevice();
}

// --- the handle ------------------------------------------------------------------------------

const TOKEN = Symbol('flashraw.internal');

/** Open `buf` (an `InputBuffer` of `eng`, consumed) in `eng`; resolves to its `RawFile`. */
async function openIn(eng, buf, json) {
  if (eng.pool) {
    // Parsing may use the thread pool: run it on the coordinator (input ownership moves).
    const input = wcall(() => buf.intoRaw(), eng);
    let ptr;
    try {
      ptr = await eng.pool.call('open', { input, options: json });
    } catch (e) {
      throw toError(e, undefined, eng);
    }
    return eng.wb.rawFileFromRaw(ptr);
  }
  return wcall(() => eng.wb.open(buf, json), eng);
}

/** The error of a build without the JPEG XL codec meeting a JPEG XL DNG. */
const isJpegXlUnsupported = (e) => e?.kind === 'UnsupportedCompression' && /JPEG XL/i.test(e.message);

function jpegXlError(why, cause) {
  return new UnsupportedCompressionError(
    `this JPEG XL DNG needs the JPEG XL variant of FlashRaw (\`flashraw_simd_jxl.wasm\`), which is not available: ${why}` +
      (build.jpegxlMode === 'never' ? '; `init({ jpegxl: "auto" })` (the default) loads it when a file needs it' : ''),
    cause ? { cause } : undefined,
  );
}

/** An opened RAW file. Create with `FlashRaw.open()`; release with `close()`. */
export class FlashRaw {
  #raw;
  #eng; // engine holding #raw (the primary one, or the JPEG XL one after a switch)
  #openJson; // open options, to reopen the file in the JPEG XL engine
  #retired = []; // RawFiles replaced by a switch while zero-copy previews still point into them
  #switching = null;
  #cache = {};
  #broken = false;

  /** @internal use FlashRaw.open() */
  constructor(token, raw, eng, json) {
    if (token !== TOKEN) throw new TypeError('use `await FlashRaw.open(input)` to create a FlashRaw');
    this.#raw = raw;
    this.#eng = eng;
    this.#openJson = json;
  }

  /** Open a RAW file from bytes, a Blob/File, or (Node.js) a file path. */
  static async open(input, options) {
    const opts = checkObject(options, 'options');
    await init(opts.init);
    const eng = primary;
    const buf = await inputBuffer(eng, input);
    const json = JSON.stringify({ limits: opts.limits ?? null });
    return new this(TOKEN, await openIn(eng, buf, json), eng, json);
  }

  /** The WASM binary serving this file (`'simd'`, `'threads'`, `'simd-jxl'`, ...). */
  get binary() {
    return this.#eng.build.binary;
  }

  /**
   * Make sure the engine of this handle can decode the file: a JPEG XL DNG opened in a build
   * without the codec moves to the JPEG XL engine (loaded on first need, then shared). The
   * metadata, previews and thumbnails never need it.
   */
  #ensureCodec() {
    if (this.#eng.build.jpegxl || this.frameInfo.compression !== 'jpeg-xl') return null;
    if (build.jpegxlMode === 'never') return null; // the decoder reports it (see #decode)
    return this.#switchToJpegXl();
  }

  #switchToJpegXl() {
    this.#switching ??= (async () => {
      let jx;
      try {
        jx = await loadJpegXl();
      } catch (e) {
        throw jpegXlError(e.message, e);
      }
      const from = this.#eng;
      const old = this.#raw;
      if (!old) throw new ClosedError('this FlashRaw handle is closed');
      if (from === jx) return;
      const len = wcall(() => old.inputLen(), from);
      const buf = wcall(() => new jx.wb.InputBuffer(len), jx);
      heap(jx, Uint8Array, buf.ptr, len).set(heap(from, Uint8Array, old.inputPtr(), len));
      const raw = await openIn(jx, buf, this.#openJson);
      if (this.#raw !== old) {
        raw.free(); // closed meanwhile
        throw new ClosedError('this FlashRaw handle is closed');
      }
      this.#raw = raw;
      this.#eng = jx;
      // Pinned zero-copy previews still point into the old copy: keep it until close().
      if (live.has(this)) this.#retired.push({ eng: from, raw: old });
      else old.free();
    })().finally(() => {
      this.#switching = null;
    });
    return this.#switching;
  }

  /** Open a file by path (Node.js only). */
  static async openFile(path, options) {
    if (!platform.fileSource) throw new NotAvailableError('`FlashRaw.openFile()` is only available in Node.js');
    return this.open(path, options);
  }

  /** Initialise the WASM module (see `init`). */
  static init(options) {
    return init(options);
  }

  #call(fn) {
    if (!this.#raw) throw new ClosedError('this FlashRaw handle is closed');
    if (this.#broken) throw new InternalError('this FlashRaw handle is unusable after an internal error');
    try {
      return fn(this.#raw);
    } catch (e) {
      const err = toError(e);
      if (err.kind === 'Internal') this.#broken = true;
      throw err;
    }
  }

  async #callAsync(fn) {
    if (!this.#raw) throw new ClosedError('this FlashRaw handle is closed');
    if (this.#broken) throw new InternalError('this FlashRaw handle is unusable after an internal error');
    try {
      return await fn(this.#raw);
    } catch (e) {
      const err = toError(e);
      if (err.kind === 'Internal') this.#broken = true;
      throw err;
    }
  }

  /**
   * Threaded build: run `op` on the coordinator; resolves to the resulting wasm `Frame`. The job
   * holds its own strong reference to the file, so `close()` during a decode is safe.
   */
  async #job(op, args) {
    if (!this.#raw) throw new ClosedError('this FlashRaw handle is closed');
    if (this.#broken) throw new InternalError('this FlashRaw handle is unusable after an internal error');
    const { wb, pool } = this.#eng;
    const shared = wcall(() => this.#raw.share(), this.#eng);
    let released = false;
    try {
      const ptr = await pool.call(op, { ...args, shared });
      released = true;
      wb.releaseShared(shared);
      return wb.frameFromRaw(ptr);
    } catch (e) {
      const err = toError(e, undefined, this.#eng);
      if (err.kind === 'Internal') this.#broken = true;
      throw err;
    } finally {
      // After a thread crash the coordinator may still hold it: leak rather than race.
      if (!released && !pool.broken) wb.releaseShared(shared);
    }
  }

  #json(key, fn) {
    return (this.#cache[key] ??= JSON.parse(this.#call(fn)));
  }

  get closed() {
    return !this.#raw;
  }
  /** Normalised metadata (no pixel decoding). Parsed once and cached. */
  get metadata() {
    return this.#json('metadata', (r) => r.metadataJson());
  }
  /** Geometry and calibration of the sensor buffer. */
  get frameInfo() {
    return this.#json('frameInfo', (r) => r.frameInfoJson());
  }
  /** What this file supports (random access, tiles, previews, ...). */
  get capabilities() {
    return this.#json('capabilities', (r) => r.capabilitiesJson());
  }
  /** Independently decodable tiles/strips (for `decodeTile`). */
  get tiles() {
    return this.#json('tiles', (r) => r.unitsJson());
  }
  get format() {
    return this.metadata.format;
  }
  get previews() {
    return this.metadata.previews;
  }
  /** Size of the input in bytes. */
  get byteLength() {
    return this.#call((r) => r.inputLen());
  }
  /** Milliseconds spent parsing the container in `open`. */
  get parseMs() {
    return this.#call((r) => r.parseMs());
  }

  /** Largest embedded preview, or `null` if the file has none. */
  async preview(options) {
    return this.#preview('largest', options);
  }
  /** Smallest embedded preview (thumbnail), or `null` if the file has none. */
  async thumbnail(options) {
    return this.#preview('smallest', options);
  }

  #preview(which, options) {
    const o = checkObject(options, 'preview options');
    const sel = o.index !== undefined && o.index !== null ? String(o.index) : which;
    if (o.index !== undefined && o.index !== null && !(Number.isInteger(o.index) && o.index >= 0)) {
      throw new InvalidArgumentError('preview index must be a non-negative integer');
    }
    if ((o.index ?? null) === null && this.metadata.previews.length === 0) return null;
    const format = o.format ?? 'encoded';
    const eng = this.#eng;
    if (format === 'rgba8') {
      const t0 = now();
      const done = (f) => {
        try {
          const info = JSON.parse(f.infoJson());
          const data = heap(eng, Uint8ClampedArray, f.ptr, f.len).slice();
          return { ...info, format: 'rgba8', data, decodeMs: now() - t0 };
        } finally {
          f.free();
        }
      };
      if (eng.pool) return this.#job('preview', { which: sel }).then(done);
      return done(this.#call((r) => r.decodePreview(sel)));
    }
    if (format !== 'encoded') throw new InvalidArgumentError(`unknown preview format ${JSON.stringify(format)} (expected "encoded" or "rgba8")`);
    const [ptr, len, json] = this.#call((r) => r.previewSpan(sel));
    const info = { ...JSON.parse(json), format: 'encoded' };
    if (!o.zeroCopy) return { ...info, data: heap(eng, Uint8Array, ptr, len).slice() };
    // The view points into the input buffer owned by this handle: pin it until close().
    live.add(this);
    let view = null;
    return Object.defineProperty(info, 'data', {
      enumerable: true,
      get: () => {
        if (!this.#raw) throw new ClosedError('zero-copy preview used after `close()`');
        if (!view || view.buffer !== eng.wasm.memory.buffer) view = heap(eng, Uint8Array, ptr, len);
        return view;
      },
    });
  }

  /** Decode the sensor data. */
  async decode(options) {
    return this.#decode(null, checkObject(options, 'decode options'));
  }
  /** Decode only `region` (sensor-buffer coordinates). */
  async decodeRegion(region, options) {
    return this.#decode(null, { ...checkObject(options, 'decode options'), region: checkObject(region, 'region') });
  }
  /** Decode one tile/strip by index (see `tiles`). */
  async decodeTile(index, options) {
    if (!Number.isInteger(index) || index < 0) throw new InvalidArgumentError('tile index must be a non-negative integer');
    return this.#decode(index, checkObject(options, 'decode options'));
  }

  async #decode(tile, options) {
    const t0 = now();
    const { backend = 'auto', fallback = 'cpu', zeroCopy = false, init: _init, ...rest } = options;
    if (fallback !== 'cpu' && fallback !== 'error') {
      throw new InvalidArgumentError(`\`fallback\` must be "cpu" or "error", got ${JSON.stringify(fallback)}`);
    }
    if (!this.#raw) throw new ClosedError('this FlashRaw handle is closed');
    // Only a pending switch to the JPEG XL engine awaits here: otherwise the decode job takes
    // its reference to the file synchronously (close() during a decode stays safe).
    const switching = this.#ensureCodec();
    if (switching) await switching;
    const extra = (fallbackReason) => ({ requested: backend, fallbackReason, zeroCopy: !!zeroCopy, t0 });
    let effective = backend;
    let fallbackReason = null;
    const toCpu = (why) => {
      if (fallback === 'error') throw new GpuUnavailableError(why);
      fallbackReason = why;
      if (isGpu(backend)) effective = 'auto';
      // GPU-resident outputs fall back to their host equivalent ('gpu-rgba8' -> 'rgba8').
      if (isGpuOutput(rest.output)) rest.output = rest.output.slice(4);
    };
    if (isGpu(backend) || isGpuOutput(rest.output)) {
      if (!this.#raw) throw new ClosedError('this FlashRaw handle is closed');
      const { reason } =
        this.#eng === primary
          ? await gpuStatus()
          : { reason: `this file is decoded by the ${this.#eng.build.binary} build, which has no WebGPU backend` };
      if (reason) toCpu(reason);
      else {
        // WebGPU build: the asynchronous GPU decode (the browser cannot block on the GPU).
        const json = JSON.stringify({ ...rest, backend: 'gpu' });
        try {
          const r = await this.#callAsync((raw) => raw.decodeGpu(tile ?? -1, json));
          if (r instanceof wb.GpuOutput) {
            const info = JSON.parse(r.infoJson());
            info.backend = { ...info.backend, requested: backend, fallbackReason: null, binary: build.binary, jpegxl: false };
            info.timings = { ...info.timings, copyMs: 0, wallMs: now() - t0 };
            return new GpuFrame(info, r);
          }
          return makeFrame(primary, r, extra(null));
        } catch (e) {
          if (e.kind !== 'GpuUnavailable') throw e;
          toCpu(e.message);
        }
      }
    }
    const json = JSON.stringify({ ...rest, backend: effective });
    const cpu = () =>
      this.#eng.pool
        ? this.#job('decode', { tile: tile ?? -1, options: json })
        : this.#call((r) => (tile === null ? r.decode(json) : r.decodeTile(tile, json)));
    let frame;
    try {
      frame = await cpu();
    } catch (e) {
      // A JPEG XL payload the compression probe did not announce: switch engines and retry.
      if (!isJpegXlUnsupported(e) || this.#eng.build.jpegxl) throw e;
      if (build.jpegxlMode === 'never') throw jpegXlError(build.jpegxlReason, e);
      await this.#switchToJpegXl();
      frame = await cpu();
    }
    return makeFrame(this.#eng, frame, extra(fallbackReason));
  }

  /** Release the WASM memory held by this handle (idempotent). */
  close() {
    const r = this.#raw;
    this.#raw = null;
    this.#cache = {};
    live.delete(this);
    for (const old of [r, ...this.#retired.splice(0).map((x) => x.raw)]) {
      if (!old) continue;
      try {
        old.free();
      } catch {}
    }
  }
  [dispose]() {
    this.close();
  }
}

// --- one-shot helpers -------------------------------------------------------------------------

async function withRaw(input, options, fn) {
  const raw = await FlashRaw.open(input, options);
  try {
    return await fn(raw);
  } finally {
    raw.close();
  }
}

/** Open, decode and close. */
export function decodeRaw(input, options) {
  const o = checkObject(options, 'options');
  return withRaw(input, o, (r) => r.decode({ ...o, zeroCopy: false }));
}

/** Metadata only (no pixel decoding). */
export function readMetadata(input, options) {
  return withRaw(input, options, (r) => r.metadata);
}

/** Largest embedded preview (or null). */
export function extractPreview(input, options) {
  const o = checkObject(options, 'options');
  return withRaw(input, o, (r) => r.preview({ ...o, zeroCopy: false }));
}

/** Smallest embedded preview (or null). */
export function extractThumbnail(input, options) {
  const o = checkObject(options, 'options');
  return withRaw(input, o, (r) => r.thumbnail({ ...o, zeroCopy: false }));
}

const PROBE_BYTES = 1 << 20;

function detectBytes(b) {
  return wcall(() => wb.detect(b)) ?? null;
}

/**
 * Load the JPEG XL variant now (it is otherwise loaded on the first decode of a JPEG XL DNG).
 * Resolves to its binary name (`'simd-jxl'` / `'threads-jxl'`; the selected binary when it has
 * the codec already); rejects with `NotAvailableError` when it cannot be used.
 */
export async function loadJpegXlSupport() {
  await init();
  return (await loadJpegXl()).build.binary;
}

/**
 * Cheap format detection: the container format name ("DNG", "CR3", ...) or null. Only the
 * first MiB is examined unless the header points further into the file.
 */
export async function detectFormat(input) {
  await init();
  if (isBlob(input)) {
    const head = new Uint8Array(await input.slice(0, PROBE_BYTES).arrayBuffer());
    const f = detectBytes(head);
    if (f || input.size <= PROBE_BYTES) return f;
    return detectBytes(new Uint8Array(await input.arrayBuffer()));
  }
  if ((typeof input === 'string' || input instanceof URL) && platform.readHead) {
    const { head, size } = await platform.readHead(input, PROBE_BYTES);
    const f = detectBytes(head);
    if (f || size <= PROBE_BYTES) return f;
    return detectBytes((await platform.readHead(input, size)).head);
  }
  const b = asBytes(input);
  if (!b) throw new InvalidArgumentError(`unsupported input type ${tag(input)}`);
  const f = detectBytes(b.length > PROBE_BYTES ? b.subarray(0, PROBE_BYTES) : b);
  return f || b.length <= PROBE_BYTES ? f : detectBytes(b);
}

/** Whether the input looks like a RAW file FlashRaw understands. */
export async function supportsFile(input) {
  return (await detectFormat(input)) !== null;
}

/** Runtime/build capabilities (initialises the module if needed). */
export async function getCapabilities() {
  let initError = null;
  try {
    await init();
  } catch (e) {
    initError = e;
  }
  const nav = typeof navigator !== 'undefined' ? navigator : undefined;
  const gpu = build ? await gpuStatus() : { reason: 'module not loaded', adapter: null };
  return {
    version: build?.version ?? null,
    platform: platform.name,
    wasm: {
      loaded: !!wasm,
      variant: build?.variant ?? null,
      binary: build?.binary ?? null,
      initMs: build?.initMs ?? null,
      error: initError ? initError.message : null,
    },
    simd: {
      supported: simdSupported(),
      active: build?.simd ?? false,
      level: build?.simdLevel ?? null,
      fallbackReason: build?.fallbackReason ?? null,
    },
    threads: {
      wasmThreads: build?.threads ?? false,
      count: threads && !threads.broken ? threads.count : 0,
      reason: build ? (build.threadsReason ?? (threads?.broken ? threads.broken.message : null)) : 'module not loaded',
      sharedArrayBuffer: typeof SharedArrayBuffer === 'function',
      crossOriginIsolated: globalThis.crossOriginIsolated === true,
      hardwareConcurrency: platform.hardwareConcurrency?.() ?? nav?.hardwareConcurrency ?? 1,
      workers: platform.workers?.() ?? typeof Worker === 'function',
    },
    gpu: {
      compiled: build?.gpu ?? false,
      webgpu: !!nav?.gpu,
      available: gpu.reason === null,
      reason: gpu.reason,
      adapter: gpu.adapter,
    },
    jpegxl: {
      mode: build?.jpegxlMode ?? null,
      compiled: build?.jpegxl ?? false,
      loaded: !!jxl,
      binary: jxl?.build.binary ?? null,
      available: !!build && !build.jpegxlReason,
      reason: build ? (build.jpegxlReason ?? jxlError) : 'module not loaded',
    },
    outputs: [...OUTPUTS, ...(build?.gpu ? GPU_OUTPUTS : [])],
    backends: [...BACKENDS],
    limits: { maxSamples: build?.maxSamples ?? null },
  };
}

/** @internal Raw module access for tests and benchmarks. */
export function _internals() {
  ensureInit();
  return {
    wasm,
    build: { ...build },
    liveCount: live.size,
    threads: threads?.count ?? 0,
    jpegxl: { loads: jxlLoads, binary: jxl?.build.binary ?? null, threads: jxl?.pool?.count ?? 0 },
  };
}
