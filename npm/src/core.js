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

import initBindgen, * as wb from './flashraw_bindgen.js';
import {
  FlashRawError,
  ClosedError,
  GpuUnavailableError,
  InternalError,
  InvalidArgumentError,
  IoError,
  NotAvailableError,
  errorFromKind,
} from './errors.js';

export * from './errors.js';

export const OUTPUTS = Object.freeze(['raw-u16', 'raw-f32', 'normalized', 'linear-rgb', 'rgba8']);
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
let wasm = null; // wasm-bindgen exports (incl. `memory`)
let build = null; // { version, simd, simdLevel, gpu, gpuAvailable, threads, maxSamples, variant, initMs, ... }
let initPromise = null;

/** @internal Called once by the platform entry point. */
export function _setPlatform(p) {
  platform = p;
}

/**
 * Load and instantiate the WebAssembly module. Called implicitly by every API; call it
 * explicitly to control which binary is used or to warm up early. Idempotent: options are only
 * honoured by the first call.
 */
export function init(options = {}) {
  if (!initPromise) {
    initPromise = doInit(options ?? {}).catch((e) => {
      initPromise = null;
      throw e;
    });
  }
  return initPromise;
}

async function doInit(options) {
  const t0 = now();
  const engineSimd = simdSupported();
  const pref = options.simd ?? platform.defaultSimd?.() ?? 'auto';
  let variant = pref === false || !engineSimd ? 'fallback' : 'simd';
  const reason =
    pref === false ? 'disabled by option' : !engineSimd ? 'engine lacks WebAssembly SIMD' : null;
  let exports;
  try {
    const source = options.wasm ?? (await platform.load(variant, options));
    exports = await initBindgen({ module_or_path: await source });
  } catch (e) {
    if (variant !== 'simd' || options.wasm) throw toError(e, 'failed to load the FlashRaw WebAssembly module');
    // A SIMD-capable probe but a failing SIMD binary (exotic engines, broken CDN): retry scalar.
    variant = 'fallback';
    exports = await initBindgen({ module_or_path: await platform.load(variant, options) }).catch((e2) => {
      throw toError(e2, 'failed to load the FlashRaw WebAssembly module');
    });
  }
  wasm = exports;
  build = {
    ...JSON.parse(wb.buildInfo()),
    variant,
    simdSupported: engineSimd,
    fallbackReason: variant === 'fallback' ? reason ?? 'SIMD binary failed to load' : null,
    initMs: now() - t0,
  };
  return { ...build };
}

function ensureInit() {
  if (!wasm) throw new InternalError('FlashRaw is not initialised; await init() first');
}

function heap(Ctor, ptr, len) {
  return new Ctor(wasm.memory.buffer, ptr, len);
}

/** Convert anything thrown by wasm-bindgen / the WASM module into a FlashRawError. */
function toError(e, context) {
  if (e instanceof FlashRawError) return e;
  if (e && typeof e === 'object' && typeof e.kind === 'string') return errorFromKind(e.kind, e.message);
  if (typeof WebAssembly === 'object' && e instanceof WebAssembly.RuntimeError) {
    let panic = null;
    try {
      panic = wb.takeLastPanic() ?? null;
    } catch {}
    const hint = panic ? `panic: ${panic}` : `${e.message} (likely out of memory)`;
    return new InternalError(`FlashRaw WASM trap: ${hint}`, { cause: e });
  }
  const msg = e && typeof e === 'object' && 'message' in e ? e.message : String(e);
  return new InternalError(context ? `${context}: ${msg}` : msg, { cause: e });
}

function wcall(fn) {
  try {
    return fn();
  } catch (e) {
    throw toError(e);
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

function copyIn(bytes) {
  const buf = wcall(() => new wb.InputBuffer(bytes.length));
  heap(Uint8Array, buf.ptr, bytes.length).set(bytes);
  return buf;
}

/** Stream chunks straight into WASM memory (one copy, no full-size JS buffer). */
async function streamIn(size, chunks) {
  let buf;
  try {
    buf = wcall(() => new wb.InputBuffer(size));
  } catch (e) {
    await chunks.return?.(); // release the underlying file handle / reader
    throw e;
  }
  let off = 0;
  try {
    for await (const c of chunks) {
      if (off + c.length > size) throw new IoError(`input is longer than its announced size (${size} bytes)`);
      // Re-derive the view each chunk: memory may have grown (and detached old views) meanwhile.
      heap(Uint8Array, buf.ptr + off, c.length).set(c);
      off += c.length;
    }
    if (off !== size) throw new IoError(`input ended after ${off} of ${size} bytes`);
  } catch (e) {
    buf.free();
    throw e instanceof FlashRawError ? e : new IoError(`failed to read input: ${e?.message ?? e}`, { cause: e });
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

async function inputBuffer(input) {
  const bytes = asBytes(input);
  if (bytes) return copyIn(bytes);
  if (isBlob(input)) return streamIn(input.size, blobChunks(input));
  if (typeof input === 'string' || input instanceof URL) {
    if (!platform.fileSource) {
      throw new InvalidArgumentError('file paths are only supported in Node.js; pass bytes, a Blob or a File');
    }
    const { size, chunks } = await platform.fileSource(input);
    return streamIn(size, chunks);
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

const ARRAYS = { u16: Uint16Array, f32: Float32Array, u8: Uint8Array };
// Zero-copy frames / pinned handles are strongly referenced until freed: a bare view does not
// keep its owner alive, so GC must never free WASM memory a view still points at.
const live = new Set();

/** A decoded frame. Plain data fields plus `data` (a typed array). */
export class DecodedFrame {
  #data;
  #view; // { frame, Ctor, ptr, len } while zero-copy
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
      if (!this.#data || this.#data.buffer !== wasm.memory.buffer) this.#data = heap(v.Ctor, v.ptr, v.len);
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
    if (this.output !== 'rgba8') throw new InvalidArgumentError('toImageData() requires output "rgba8"');
    if (typeof ImageData === 'undefined') throw new NotAvailableError('ImageData is not available in this environment');
    return new ImageData(this.data, this.width, this.height);
  }
  /** Plain, structured-clone friendly object (`data` included). */
  toJSON() {
    return { ...this, data: undefined };
  }
}

function makeFrame(frame, extra) {
  const t0 = now();
  const info = JSON.parse(frame.infoJson());
  const kind = frame.kind;
  const Ctor = kind === 'u8' && info.output === 'rgba8' ? Uint8ClampedArray : ARRAYS[kind];
  const ptr = frame.ptr;
  const len = frame.len;
  info.dataType = kind;
  info.backend = { ...info.backend, requested: extra.requested, fallbackReason: extra.fallbackReason ?? null };
  info.timings = { ...info.timings, copyMs: 0, wallMs: 0 };
  let out;
  if (extra.zeroCopy) {
    out = new DecodedFrame(info, null, { frame, Ctor, ptr, len });
  } else {
    const data = heap(Ctor, ptr, len).slice();
    frame.free();
    info.timings.copyMs = now() - t0;
    out = new DecodedFrame(info, data, null);
  }
  out.timings.wallMs = now() - extra.t0;
  return out;
}

function gpuUnavailableReason() {
  if (!build?.gpu) return 'this FlashRaw WASM binary was built without the `gpu` feature (WebGPU backend not compiled in)';
  if (typeof navigator === 'undefined' || !navigator.gpu) {
    return 'WebGPU is not available in this environment (navigator.gpu is undefined)';
  }
  if (!build.gpuAvailable) return 'no WebGPU adapter is available';
  return null;
}

const isGpu = (b) => typeof b === 'string' && ['gpu', 'webgpu'].includes(b.toLowerCase());

// --- the handle ------------------------------------------------------------------------------

const TOKEN = Symbol('flashraw.internal');

/** An opened RAW file. Create with `FlashRaw.open()`; release with `close()`. */
export class FlashRaw {
  #raw;
  #cache = {};
  #broken = false;

  /** @internal use FlashRaw.open() */
  constructor(token, raw) {
    if (token !== TOKEN) throw new TypeError('use `await FlashRaw.open(input)` to create a FlashRaw');
    this.#raw = raw;
  }

  /** Open a RAW file from bytes, a Blob/File, or (Node.js) a file path. */
  static async open(input, options) {
    const opts = checkObject(options, 'options');
    await init(opts.init);
    const buf = await inputBuffer(input);
    const raw = wcall(() => wb.open(buf, JSON.stringify({ limits: opts.limits ?? null })));
    return new this(TOKEN, raw);
  }

  /** Open a file by path (Node.js only). */
  static async openFile(path, options) {
    if (!platform.fileSource) throw new NotAvailableError('FlashRaw.openFile() is only available in Node.js');
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
    if (o.index === undefined && this.metadata.previews.length === 0) return null;
    const format = o.format ?? 'encoded';
    if (format === 'rgba8') {
      const t0 = now();
      const f = this.#call((r) => r.decodePreview(sel));
      try {
        const info = JSON.parse(f.infoJson());
        const data = heap(Uint8ClampedArray, f.ptr, f.len).slice();
        return { ...info, format: 'rgba8', data, decodeMs: now() - t0 };
      } finally {
        f.free();
      }
    }
    if (format !== 'encoded') throw new InvalidArgumentError(`unknown preview format "${format}" (encoded | rgba8)`);
    const [ptr, len, json] = this.#call((r) => r.previewSpan(sel));
    const info = { ...JSON.parse(json), format: 'encoded' };
    if (!o.zeroCopy) return { ...info, data: heap(Uint8Array, ptr, len).slice() };
    // The view points into the input buffer owned by this handle: pin it until close().
    live.add(this);
    let view = null;
    return Object.defineProperty(info, 'data', {
      enumerable: true,
      get: () => {
        if (!this.#raw) throw new ClosedError('zero-copy preview used after close()');
        if (!view || view.buffer !== wasm.memory.buffer) view = heap(Uint8Array, ptr, len);
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

  #decode(tile, options) {
    const t0 = now();
    const { backend = 'auto', fallback = 'cpu', zeroCopy = false, init: _init, ...rest } = options;
    if (fallback !== 'cpu' && fallback !== 'error') {
      throw new InvalidArgumentError(`fallback must be "cpu" or "error", got ${JSON.stringify(fallback)}`);
    }
    let effective = backend;
    let fallbackReason = null;
    if (isGpu(backend)) {
      const why = gpuUnavailableReason();
      if (why) {
        if (fallback === 'error') throw new GpuUnavailableError(why);
        effective = 'auto';
        fallbackReason = why;
      }
    }
    const run = (b) => {
      const json = JSON.stringify({ ...rest, backend: b });
      return this.#call((r) => (tile === null ? r.decode(json) : r.decodeTile(tile, json)));
    };
    let frame;
    try {
      frame = run(effective);
    } catch (e) {
      if (e.kind !== 'GpuUnavailable' || !isGpu(effective) || fallback !== 'cpu') throw e;
      fallbackReason = e.message;
      frame = run('auto');
    }
    return makeFrame(frame, { requested: backend, fallbackReason, zeroCopy: !!zeroCopy, t0 });
  }

  /** Release the WASM memory held by this handle (idempotent). */
  close() {
    const r = this.#raw;
    this.#raw = null;
    this.#cache = {};
    live.delete(this);
    if (r) {
      try {
        r.free();
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
  const gpuReason = build ? gpuUnavailableReason() : 'module not loaded';
  return {
    version: build?.version ?? null,
    platform: platform.name,
    wasm: {
      loaded: !!wasm,
      variant: build?.variant ?? null,
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
      sharedArrayBuffer: typeof SharedArrayBuffer === 'function',
      crossOriginIsolated: globalThis.crossOriginIsolated === true,
      hardwareConcurrency: platform.hardwareConcurrency?.() ?? nav?.hardwareConcurrency ?? 1,
      workers: platform.workers?.() ?? typeof Worker === 'function',
    },
    gpu: {
      compiled: build?.gpu ?? false,
      webgpu: !!nav?.gpu,
      available: gpuReason === null,
      reason: gpuReason,
    },
    outputs: [...OUTPUTS],
    backends: [...BACKENDS],
    limits: { maxSamples: build?.maxSamples ?? null },
  };
}

/** @internal Raw module access for tests and benchmarks. */
export function _internals() {
  ensureInit();
  return { wasm, build: { ...build }, liveCount: live.size };
}
