// Worker pool shared by the browser (Web Worker) and Node.js (worker_threads) helpers.
//
// Each worker runs its own FlashRaw instance (the single-threaded build unless the pool's
// `init` options ask for threads; the multithreaded build parallelises *one* decode instead, see
// threads.js). Decoding off the main thread keeps UIs responsive, and a pool decodes several
// files concurrently.
//
// Data movement: results are copied out of WASM memory inside the worker and their buffers are
// *transferred* (not cloned) to the caller. Inputs are cloned unless `transfer: true` is given
// (then the caller's ArrayBuffer is moved and becomes detached). Blobs/Files are passed by
// reference and read inside the worker; Node file paths are opened by the worker itself.

import { errorFromJSON, InvalidArgumentError, ClosedError, InternalError } from './errors.js';

// ----------------------------------------------------------------------------- worker side

function transferablesOf(value) {
  const out = [];
  const visit = (v) => {
    if (!v || typeof v !== 'object') return;
    if (ArrayBuffer.isView(v)) {
      if (Object.prototype.toString.call(v.buffer) === '[object ArrayBuffer]') out.push(v.buffer);
      return;
    }
    if (Array.isArray(v)) v.forEach(visit);
  };
  visit(value?.data);
  return out;
}

function plainFrame(f) {
  if (!f) return f;
  const data = f.data;
  return { ...f, data };
}

/**
 * Serve requests in a worker. `api` is the flashraw module of the worker's platform.
 * @param {{ post(msg: any, transfer?: any[]): void, listen(cb: (msg: any) => void): void }} port
 */
export function serve(api, port) {
  const handles = new Map();
  let next = 1;
  const ops = {
    async init(args) {
      return api.init(args ?? {});
    },
    async capabilities() {
      return api.getCapabilities();
    },
    async detect({ input }) {
      return api.detectFormat(input);
    },
    async oneshot({ task, input, options }) {
      const o = { ...(options ?? {}), zeroCopy: false };
      switch (task) {
        case 'decode':
          return plainFrame(await api.decodeRaw(input, o));
        case 'metadata':
          return api.readMetadata(input, o);
        case 'preview':
          return api.extractPreview(input, o);
        case 'thumbnail':
          return api.extractThumbnail(input, o);
        default:
          throw new InvalidArgumentError(`unknown task ${task}`);
      }
    },
    async open({ input, options }) {
      const raw = await api.FlashRaw.open(input, options);
      const id = next++;
      handles.set(id, raw);
      return {
        handle: id,
        metadata: raw.metadata,
        frameInfo: raw.frameInfo,
        capabilities: raw.capabilities,
        tiles: raw.tiles,
        byteLength: raw.byteLength,
        parseMs: raw.parseMs,
      };
    },
    async call({ handle, method, args }) {
      const raw = handles.get(handle);
      if (!raw) throw new ClosedError('remote FlashRaw handle is closed');
      const a = args ?? [];
      switch (method) {
        case 'decode':
          return plainFrame(await raw.decode({ ...(a[0] ?? {}), zeroCopy: false }));
        case 'decodeRegion':
          return plainFrame(await raw.decodeRegion(a[0], { ...(a[1] ?? {}), zeroCopy: false }));
        case 'decodeTile':
          return plainFrame(await raw.decodeTile(a[0], { ...(a[1] ?? {}), zeroCopy: false }));
        case 'preview':
          return raw.preview({ ...(a[0] ?? {}), zeroCopy: false });
        case 'thumbnail':
          return raw.thumbnail({ ...(a[0] ?? {}), zeroCopy: false });
        default:
          throw new InvalidArgumentError(`unknown method ${method}`);
      }
    },
    async close({ handle }) {
      handles.get(handle)?.close();
      handles.delete(handle);
      return true;
    },
  };
  port.listen(async (msg) => {
    const { id, op, args } = msg ?? {};
    try {
      const fn = ops[op];
      if (!fn) throw new InvalidArgumentError(`unknown op ${op}`);
      const result = await fn(args);
      port.post({ id, ok: true, result }, transferablesOf(result));
    } catch (e) {
      const error = { kind: e?.kind ?? 'Internal', message: e?.message ?? String(e) };
      port.post({ id, ok: false, error });
    }
  });
}

// ----------------------------------------------------------------------------- caller side

function inputTransfer(input, transfer) {
  if (!transfer) return [];
  if (Object.prototype.toString.call(input) === '[object ArrayBuffer]') return [input];
  if (ArrayBuffer.isView(input) && input.byteOffset === 0 && input.byteLength === input.buffer.byteLength) {
    return [input.buffer];
  }
  return [];
}

// Remote handles dropped without close() are closed in their worker once garbage-collected.
const remoteFinalizer =
  typeof FinalizationRegistry === 'function'
    ? new FinalizationRegistry(({ pool, slot, handle }) => {
        pool._request(slot, 'close', { handle }).catch(() => {});
      })
    : null;

/** A FlashRaw handle living in a worker. Methods mirror `FlashRaw`. */
export class RemoteFlashRaw {
  #pool;
  #slot;
  #handle;
  constructor(pool, slot, info) {
    this.#pool = pool;
    this.#slot = slot;
    this.#handle = info.handle;
    this.metadata = info.metadata;
    this.frameInfo = info.frameInfo;
    this.capabilities = info.capabilities;
    this.tiles = info.tiles;
    this.byteLength = info.byteLength;
    this.parseMs = info.parseMs;
    remoteFinalizer?.register(this, { pool, slot, handle: info.handle }, this);
  }
  get format() {
    return this.metadata.format;
  }
  get previews() {
    return this.metadata.previews;
  }
  get closed() {
    return this.#handle === null;
  }
  #call(method, args) {
    if (this.#handle === null) return Promise.reject(new ClosedError('remote FlashRaw handle is closed'));
    return this.#pool._request(this.#slot, 'call', { handle: this.#handle, method, args });
  }
  decode(options) {
    return this.#call('decode', [options]);
  }
  decodeRegion(region, options) {
    return this.#call('decodeRegion', [region, options]);
  }
  decodeTile(index, options) {
    return this.#call('decodeTile', [index, options]);
  }
  preview(options) {
    return this.#call('preview', [options]);
  }
  thumbnail(options) {
    return this.#call('thumbnail', [options]);
  }
  async close() {
    const h = this.#handle;
    this.#handle = null;
    remoteFinalizer?.unregister(this);
    if (h !== null) await this.#pool._request(this.#slot, 'close', { handle: h }).catch(() => {});
  }
  [Symbol.asyncDispose ?? Symbol.for('Symbol.asyncDispose')]() {
    return this.close();
  }
}

/**
 * A pool of workers, each running its own FlashRaw WASM instance.
 * @internal construct via `createWorkerPool` from `flashraw/worker`.
 */
export class WorkerPool {
  #spawn;
  #size;
  #init;
  #slots = [];
  #nextId = 1;
  #terminated = false;

  constructor(spawn, options = {}) {
    if (options && (typeof options !== 'object' || Array.isArray(options))) {
      throw new InvalidArgumentError('pool options must be an object');
    }
    const size = options.size ?? defaultSize(options.hardwareConcurrency);
    if (!Number.isInteger(size) || size < 1) throw new InvalidArgumentError('pool size must be a positive integer');
    this.#spawn = spawn;
    this.#size = size;
    this.#init = options.init ?? null;
  }

  get size() {
    return this.#size;
  }

  /** Number of requests queued or running per worker. */
  get load() {
    return this.#slots.map((s) => s.pending.size);
  }

  #makeSlot(index) {
    const w = this.#spawn();
    const slot = { index, w, pending: new Map() };
    w.onMessage((msg) => {
      const p = slot.pending.get(msg.id);
      if (!p) return;
      slot.pending.delete(msg.id);
      if (slot.pending.size === 0) w.idle?.(true);
      if (msg.ok) p.resolve(msg.result);
      else p.reject(errorFromJSON(msg.error));
    });
    w.onError((err) => {
      // The worker died (e.g. crashed or failed to load): fail its requests and replace it.
      const e = new InternalError(`FlashRaw worker failed: ${err?.message ?? err}`, { cause: err });
      for (const p of slot.pending.values()) p.reject(e);
      slot.pending.clear();
      slot.dead = true;
      try {
        w.terminate();
      } catch {}
    });
    if (this.#init) this.#post(slot, 'init', this.#init).catch(() => {});
    return slot;
  }

  #slot(index) {
    if (this.#terminated) throw new ClosedError('worker pool has been terminated');
    if (index !== null && index !== undefined) {
      const s = this.#slots[index];
      if (!s || s.dead) throw new ClosedError('the worker owning this handle has terminated');
      return s;
    }
    // Least-loaded live worker; spawn lazily up to `size`.
    for (let i = 0; i < this.#slots.length; i++) {
      if (this.#slots[i].dead) this.#slots[i] = this.#makeSlot(i);
    }
    let best = this.#slots.reduce((a, s) => (!a || s.pending.size < a.pending.size ? s : a), null);
    if ((!best || best.pending.size > 0) && this.#slots.length < this.#size) {
      best = this.#makeSlot(this.#slots.length);
      this.#slots.push(best);
    }
    return best;
  }

  #post(slot, op, args, transfer = []) {
    const id = this.#nextId++;
    return new Promise((resolve, reject) => {
      slot.pending.set(id, { resolve, reject });
      slot.w.idle?.(false);
      try {
        slot.w.post({ id, op, args }, transfer);
      } catch (e) {
        slot.pending.delete(id);
        reject(new InvalidArgumentError(`cannot send input to worker: ${e?.message ?? e}`, { cause: e }));
      }
    });
  }

  /** @internal */
  _request(index, op, args, transfer) {
    try {
      return this.#post(this.#slot(index), op, args, transfer);
    } catch (e) {
      return Promise.reject(e);
    }
  }

  #oneshot(task, input, options) {
    const { transfer, ...rest } = options ?? {};
    return this._request(null, 'oneshot', { task, input, options: rest }, inputTransfer(input, transfer));
  }

  /** Decode in a worker. Same options as `FlashRaw#decode` (+ `transfer`). */
  decode(input, options) {
    return this.#oneshot('decode', input, options);
  }
  readMetadata(input, options) {
    return this.#oneshot('metadata', input, options);
  }
  extractPreview(input, options) {
    return this.#oneshot('preview', input, options);
  }
  extractThumbnail(input, options) {
    return this.#oneshot('thumbnail', input, options);
  }
  detectFormat(input) {
    return this._request(null, 'detect', { input });
  }
  getCapabilities() {
    return this._request(null, 'capabilities', {});
  }
  /** Open a file in one worker and keep it there for repeated calls. */
  async open(input, options) {
    const { transfer, ...rest } = options ?? {};
    const slot = this.#slot(null);
    const info = await this.#post(slot, 'open', { input, options: rest }, inputTransfer(input, transfer));
    return new RemoteFlashRaw(this, slot.index, info);
  }
  /** Stop all workers; pending requests are rejected. */
  terminate() {
    this.#terminated = true;
    for (const s of this.#slots) {
      for (const p of s.pending.values()) p.reject(new ClosedError('worker pool terminated'));
      s.pending.clear();
      try {
        s.w.terminate();
      } catch {}
    }
    this.#slots = [];
  }
  [Symbol.dispose ?? Symbol.for('Symbol.dispose')]() {
    this.terminate();
  }
}

function defaultSize(hc) {
  const n = hc ?? (typeof navigator !== 'undefined' ? navigator.hardwareConcurrency : 0) ?? 0;
  return Math.max(1, Math.min(4, (n || 2) - 1));
}
