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
//
// Requests are queued on the caller side and each worker runs one at a time: requests that are
// not tied to a worker go to whichever worker is free first; calls on a remote handle wait for
// the worker holding it. Queuing here (rather than in the worker's message queue) is what makes
// cancellation possible: an aborted request that has not started is simply dropped; one that is
// running is stopped by terminating its worker (WASM cannot be interrupted), which also closes
// the handles opened in that worker. A replacement worker is spawned on demand.

function inputTransfer(input, transfer) {
  if (!transfer) return [];
  if (Object.prototype.toString.call(input) === '[object ArrayBuffer]') return [input];
  if (ArrayBuffer.isView(input) && input.byteOffset === 0 && input.byteLength === input.buffer.byteLength) {
    return [input.buffer];
  }
  return [];
}

function abortReason(signal) {
  if (signal.reason !== undefined) return signal.reason;
  if (typeof DOMException === 'function') return new DOMException('This operation was aborted', 'AbortError');
  return Object.assign(new Error('This operation was aborted'), { name: 'AbortError' });
}

function checkSignal(signal) {
  if (signal === undefined || signal === null) return null;
  if (typeof signal !== 'object' || typeof signal.addEventListener !== 'function' || !('aborted' in signal)) {
    throw new InvalidArgumentError('signal must be an AbortSignal');
  }
  return signal;
}

/** Split the caller-only options (`signal`, `transfer`) from what is sent to the worker. */
function splitOptions(options) {
  if (!options || typeof options !== 'object' || Array.isArray(options)) {
    return { rest: options, signal: null, transfer: false };
  }
  const { signal, transfer, ...rest } = options;
  return { rest, signal: checkSignal(signal), transfer: !!transfer };
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
  /** True after `close()`, or once the worker holding the file has terminated. */
  get closed() {
    return this.#handle === null || this.#slot.dead;
  }
  #call(method, args, options) {
    if (this.#handle === null) return Promise.reject(new ClosedError('remote FlashRaw handle is closed'));
    let o;
    try {
      o = splitOptions(options);
    } catch (e) {
      return Promise.reject(e);
    }
    return this.#pool._request(this.#slot, 'call', { handle: this.#handle, method, args: [...args, o.rest] }, [], o.signal);
  }
  decode(options) {
    return this.#call('decode', [], options);
  }
  decodeRegion(region, options) {
    return this.#call('decodeRegion', [region], options);
  }
  decodeTile(index, options) {
    return this.#call('decodeTile', [index], options);
  }
  preview(options) {
    return this.#call('preview', [], options);
  }
  thumbnail(options) {
    return this.#call('thumbnail', [], options);
  }
  async close() {
    const h = this.#handle;
    this.#handle = null;
    remoteFinalizer?.unregister(this);
    if (h !== null && !this.#slot.dead) await this.#pool._request(this.#slot, 'close', { handle: h }).catch(() => {});
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
  #slots = []; // live workers: { w, queue, running, dead }
  #queue = []; // requests any worker may run
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

  /** Requests running or waiting per spawned worker (not yet assigned ones: `queued`). */
  get load() {
    return this.#slots.map((s) => s.queue.length + (s.running ? 1 : 0));
  }

  /** Requests waiting for a free worker. */
  get queued() {
    return this.#queue.length;
  }

  #makeSlot() {
    const w = this.#spawn();
    const slot = { w, queue: [], running: null, dead: false };
    w.onMessage((msg) => {
      const req = slot.running;
      if (slot.dead || !req || msg?.id !== req.id) return;
      this.#settle(slot, req);
      if (msg.ok) {
        try {
          req.resolve(req.wrap ? req.wrap(msg.result, slot) : msg.result);
        } catch (e) {
          req.reject(e);
        }
      } else {
        req.reject(errorFromJSON(msg.error));
      }
      this.#pump();
    });
    w.onError((err) => {
      // The worker died (crashed, ran out of memory, failed to load): fail what it was doing.
      const e = new InternalError(`FlashRaw worker failed: ${err?.message ?? err}`, { cause: err });
      this.#retire(slot, e, e);
    });
    this.#slots.push(slot);
    if (this.#init) {
      slot.queue.push(this.#newRequest(slot, 'init', this.#init, [], null, null, () => {}, () => {}));
    }
    return slot;
  }

  #newRequest(slot, op, args, transfer, signal, wrap, resolve, reject) {
    const req = { id: this.#nextId++, slot, op, args, transfer, signal, wrap, resolve, reject, onAbort: null };
    if (signal) {
      req.onAbort = () => this.#abort(req);
      signal.addEventListener('abort', req.onAbort, { once: true });
    }
    return req;
  }

  /** A request finished or was dropped: detach its abort listener, free its worker. */
  #settle(slot, req) {
    if (req.signal) req.signal.removeEventListener('abort', req.onAbort);
    if (slot && slot.running === req) slot.running = null;
    if (slot && !slot.dead) slot.w.idle?.(!slot.running && slot.queue.length === 0);
  }

  #abort(req) {
    const reason = abortReason(req.signal);
    const busy = this.#slots.find((s) => s.running === req);
    if (busy) {
      // Running: the only way to stop a WASM call is to terminate its worker.
      this.#retire(busy, reason, new ClosedError('the worker owning this handle was terminated to cancel a running request'));
      return;
    }
    const q = req.slot ? req.slot.queue : this.#queue;
    const i = q.indexOf(req);
    if (i < 0) return; // already settled
    q.splice(i, 1);
    this.#settle(req.slot, req);
    req.reject(reason);
  }

  /** Terminate a worker: its running request fails with `runningError`, its queue with `queuedError`. */
  #retire(slot, runningError, queuedError) {
    if (slot.dead) return;
    slot.dead = true;
    this.#slots.splice(this.#slots.indexOf(slot), 1);
    try {
      slot.w.terminate();
    } catch {}
    const r = slot.running;
    slot.running = null;
    if (r) {
      this.#settle(null, r);
      r.reject(runningError);
    }
    for (const q of slot.queue.splice(0)) {
      this.#settle(null, q);
      q.reject(queuedError);
    }
    this.#pump();
  }

  #dispatch(slot, req) {
    slot.running = req;
    slot.w.idle?.(false);
    try {
      slot.w.post({ id: req.id, op: req.op, args: req.args }, req.transfer);
    } catch (e) {
      this.#settle(slot, req);
      req.reject(new InvalidArgumentError(`cannot send input to worker: ${e?.message ?? e}`, { cause: e }));
    }
  }

  /** Start queued requests on free workers, spawning workers lazily up to `size`. */
  #pump() {
    if (this.#terminated) return;
    for (let again = true; again; ) {
      again = false;
      for (const s of this.#slots) {
        while (!s.running && s.queue.length) this.#dispatch(s, s.queue.shift());
      }
      while (this.#queue.length) {
        let s = this.#slots.find((x) => !x.running && x.queue.length === 0);
        if (!s) {
          if (this.#slots.length >= this.#size) break;
          s = this.#makeSlot();
          if (s.queue.length) {
            again = true; // start its init first
            break;
          }
        }
        this.#dispatch(s, this.#queue.shift());
      }
    }
  }

  /**
   * @internal Queue a request on `slot` (a remote handle's worker) or on any worker (null).
   * `wrap(result, slot)` post-processes a successful result.
   */
  _request(slot, op, args, transfer = [], signal = null, wrap = null) {
    if (this.#terminated) return Promise.reject(new ClosedError('worker pool has been terminated'));
    if (slot && slot.dead) return Promise.reject(new ClosedError('the worker owning this handle has terminated'));
    if (signal?.aborted) return Promise.reject(abortReason(signal));
    return new Promise((resolve, reject) => {
      const req = this.#newRequest(slot, op, args, transfer, signal, wrap, resolve, reject);
      (slot ? slot.queue : this.#queue).push(req);
      this.#pump();
    });
  }

  #unbound(op, args, input, options, wrap = null) {
    let o;
    try {
      o = splitOptions(options);
    } catch (e) {
      return Promise.reject(e);
    }
    return this._request(null, op, { ...args, input, options: o.rest }, inputTransfer(input, o.transfer), o.signal, wrap);
  }

  /** Decode in a worker. Same options as `FlashRaw#decode` (+ `transfer`, `signal`). */
  decode(input, options) {
    return this.#unbound('oneshot', { task: 'decode' }, input, options);
  }
  readMetadata(input, options) {
    return this.#unbound('oneshot', { task: 'metadata' }, input, options);
  }
  extractPreview(input, options) {
    return this.#unbound('oneshot', { task: 'preview' }, input, options);
  }
  extractThumbnail(input, options) {
    return this.#unbound('oneshot', { task: 'thumbnail' }, input, options);
  }
  detectFormat(input, options) {
    return this.#unbound('detect', {}, input, options);
  }
  getCapabilities() {
    return this._request(null, 'capabilities', {});
  }
  /** Open a file in one worker and keep it there for repeated calls. */
  open(input, options) {
    return this.#unbound('open', {}, input, options, (info, slot) => new RemoteFlashRaw(this, slot, info));
  }
  /** Stop all workers; pending requests are rejected. */
  terminate() {
    if (this.#terminated) return;
    this.#terminated = true;
    const err = new ClosedError('worker pool terminated');
    for (const r of this.#queue.splice(0)) {
      this.#settle(null, r);
      r.reject(err);
    }
    for (const s of [...this.#slots]) this.#retire(s, err, err);
  }
  [Symbol.dispose ?? Symbol.for('Symbol.dispose')]() {
    this.terminate();
  }
}

function defaultSize(hc) {
  const n = hc ?? (typeof navigator !== 'undefined' ? navigator.hardwareConcurrency : 0) ?? 0;
  return Math.max(1, Math.min(4, (n || 2) - 1));
}
