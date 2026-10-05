// Multithreaded WASM (the `flashraw_simd_threads.wasm` variant): main-thread side.
//
// The threaded binary is built with atomics + shared memory and rayon enabled in every decoder.
// Threads on the web are Web Workers (worker_threads in Node) that instantiate the *same*
// compiled module on the *same* shared `WebAssembly.Memory`:
//
//   main thread          creates/frees buffers, copies results out, never blocks
//   coordinator worker   runs open/decode/preview jobs (`job*` exports); may block, so rayon's
//                        `join`/`par_iter` from outside the pool is fine there
//   N rayon workers      `threadRun(ptr)`: each is one thread of rayon's global pool
//
// Browsers forbid blocking (`Atomics.wait` / `memory.atomic.wait`) on the main thread, which
// is why decoding never runs there in this mode. Objects cross threads as pointers; this module
// is the only code handing them around, and it guarantees each is consumed exactly once.
//
// A Rust panic aborts; inside a rayon worker that would leave the coordinator waiting forever,
// so a trap in any thread poisons the pool: pending and later calls fail with InternalError.

import { InternalError, errorFromKind } from './errors.js';

const STARTUP_TIMEOUT_MS = 30_000;

function once(worker, what) {
  return new Promise((resolve, reject) => {
    worker.onMessage((m) => {
      if (m?.type === 'ready') resolve();
      else if (m?.type === 'error') reject(new InternalError(`${what} failed to start: ${m.message}`));
    });
    worker.onError((e) => reject(new InternalError(`${what} failed to start: ${e?.message ?? e}`, { cause: e })));
  });
}

export class ThreadPool {
  #coordinator;
  #workers = [];
  #pending = new Map();
  #next = 1;
  #broken = null;

  /** Number of rayon threads (the coordinator is not counted). */
  count = 0;

  constructor(coordinator) {
    this.#coordinator = coordinator;
    this.#add(coordinator);
    this.#listen();
  }

  /**
   * Start the coordinator and `count` thread workers (one per rayon thread), then rayon's
   * global pool. The thread workers load and instantiate while the coordinator starts; each
   * becomes a rayon thread once the coordinator hands it its `ThreadBuilder` pointer.
   * @param {{ spawn: (name: string) => any, module: WebAssembly.Module, memory: WebAssembly.Memory, count: number }} o
   */
  static async start({ spawn, module, memory, count }) {
    let pool = null;
    const workers = [];
    let timer;
    const timeout = new Promise((_, reject) => {
      timer = setTimeout(
        () => reject(new InternalError(`FlashRaw threads did not start within ${STARTUP_TIMEOUT_MS / 1000} s`)),
        STARTUP_TIMEOUT_MS,
      );
    });
    try {
      const coordinator = spawn('flashraw-coordinator');
      workers.push(coordinator);
      const ready = once(coordinator, 'the FlashRaw coordinator worker');
      coordinator.post({ type: 'init', role: 'coordinator', module, memory });
      const started = [];
      const idle = [];
      const startThread = () => {
        const i = started.length;
        const w = spawn(`flashraw-thread-${i}`);
        workers.push(w);
        started.push(once(w, `FlashRaw thread ${i}`));
        w.post({ type: 'init', role: 'thread', module, memory });
        return w;
      };
      for (let i = 0; i < count; i++) idle.push(startThread());
      await Promise.race([ready, timeout]);
      pool = new ThreadPool(coordinator);
      for (const w of workers.slice(1)) pool.#add(w);
      // The coordinator builds rayon's pool; for every thread rayon wants, it forwards a
      // pointer here and the next prepared worker starts running it. The build job completes
      // once all of them are running.
      coordinator.onMessage((m) => {
        if (m?.type !== 'spawn') return;
        let w = idle.shift();
        if (!w) {
          w = startThread();
          pool.#add(w);
        }
        w.post({ type: 'run', ptr: m.ptr });
      });
      pool.count = await Promise.race([pool.call('build', { count }), timeout]);
      await Promise.race([Promise.all(started), timeout]);
      for (const w of idle) w.terminate(); // rayon asked for fewer threads than prepared
      return pool;
    } catch (e) {
      for (const w of workers) {
        try {
          w.terminate();
        } catch {}
      }
      throw e;
    } finally {
      clearTimeout(timer);
    }
  }

  #add(w) {
    this.#workers.push(w);
    w.onMessage((m) => {
      if (m?.type === 'trap') {
        this.#poison(new InternalError(`FlashRaw WASM thread trapped: ${m.panic ? `panic: ${m.panic}` : m.message}`));
      }
    });
    w.onError((e) => this.#poison(new InternalError(`FlashRaw thread failed: ${e?.message ?? e}`, { cause: e })));
  }

  #listen() {
    this.#coordinator.onMessage((m) => {
      if (m?.type !== 'result') return;
      const p = this.#pending.get(m.id);
      if (!p) return;
      this.#pending.delete(m.id);
      if (this.#pending.size === 0) this.#coordinator.idle?.(true);
      if (m.err) {
        const e = m.err;
        p.reject(
          e.trap
            ? new InternalError(`FlashRaw WASM trap: ${e.panic ? `panic: ${e.panic}` : `${e.message} (likely out of memory)`}`)
            : errorFromKind(e.kind ?? 'Internal', e.message ?? 'unknown error'),
        );
      } else {
        p.resolve(m.ok);
      }
    });
  }

  #poison(err) {
    if (this.#broken) return;
    this.#broken = err;
    for (const p of this.#pending.values()) p.reject(err);
    this.#pending.clear();
    this.terminate();
  }

  /** Error that made the pool unusable, or null. */
  get broken() {
    return this.#broken;
  }

  /** Run a job on the coordinator. Resolves to the pointer/number the job returns. */
  call(op, args) {
    if (this.#broken) return Promise.reject(this.#broken);
    const id = this.#next++;
    return new Promise((resolve, reject) => {
      this.#pending.set(id, { resolve, reject });
      if (this.#pending.size === 1) this.#coordinator.idle?.(false);
      this.#coordinator.post({ type: 'job', id, op, args });
    });
  }

  terminate() {
    for (const w of this.#workers) {
      try {
        w.terminate();
      } catch {}
    }
  }
}

// ------------------------------------------------------------------------------- worker side

/**
 * Body of a thread worker (`thread-worker.browser.js` / `thread-worker.node.js`).
 * @param {any} glue the threaded wasm-bindgen glue module
 * @param {{ post(msg: any): void, listen(cb: (msg: any) => void): void }} port
 */
export function runThreadWorker(glue, port) {
  const panicOf = () => {
    try {
      return glue.takeLastPanic() ?? null;
    } catch {
      return null;
    }
  };
  let role = null;
  port.listen((m) => {
    if (m?.type === 'init') {
      try {
        glue.initSync({ module: m.module, memory: m.memory });
      } catch (e) {
        port.post({ type: 'error', message: String(e?.message ?? e) });
        return;
      }
      role = m.role;
      port.post({ type: 'ready' });
      return;
    }
    if (m?.type === 'run' && role === 'thread') {
      // Let the current message settle, then become a rayon thread for good.
      setTimeout(() => {
        try {
          glue.threadRun(m.ptr);
        } catch (e) {
          port.post({ type: 'trap', message: String(e?.message ?? e), panic: panicOf() });
        }
      }, 0);
      return;
    }
    if (m?.type !== 'job' || role !== 'coordinator') return;
    const { id, op, args: a } = m;
    try {
      let ok;
      switch (op) {
        case 'build':
          ok = glue.threadPoolBuild(a.count, (ptr) => port.post({ type: 'spawn', ptr }));
          break;
        case 'open':
          ok = glue.jobOpen(a.input, a.options);
          break;
        case 'decode':
          ok = glue.jobDecode(a.shared, a.tile, a.options);
          break;
        case 'preview':
          ok = glue.jobDecodePreview(a.shared, a.which);
          break;
        default:
          throw new Error(`unknown job ${op}`);
      }
      port.post({ type: 'result', id, ok });
    } catch (e) {
      const trap = typeof WebAssembly === 'object' && e instanceof WebAssembly.RuntimeError;
      port.post({
        type: 'result',
        id,
        err: { kind: trap ? 'Internal' : (e?.kind ?? 'Internal'), message: String(e?.message ?? e), trap, panic: trap ? panicOf() : null },
      });
    }
  });
}
