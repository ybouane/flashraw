// Node.js entry point: WASM loaded from disk, file paths accepted everywhere an input is.
//
// Environment: FLASHRAW_WASM_SIMD=0 forces the scalar fallback binary (benchmarks, debugging);
// FLASHRAW_WASM_THREADS=0 forces the single-threaded binary, =N sets the thread count.

import { readFile, open } from 'node:fs/promises';
import { availableParallelism, cpus } from 'node:os';
import { fileURLToPath } from 'node:url';
import { Worker } from 'node:worker_threads';
import { _setPlatform, IoError } from './core.js';

const STAGING = 8 << 20; // chunk size when streaming files into WASM memory

function ioError(e, path) {
  return new IoError(`${e.code ?? 'EIO'}: cannot read ${path}: ${e.message}`, { cause: e });
}

function toPath(p) {
  return p instanceof URL ? fileURLToPath(p) : p;
}

const FILES = {
  simd: 'flashraw_simd.wasm',
  fallback: 'flashraw.wasm',
  threads: 'flashraw_simd_threads.wasm',
  gpu: 'flashraw_simd_gpu.wasm',
};

function wasmUrl(variant, options) {
  if (options.baseUrl) return new URL(FILES[variant], options.baseUrl);
  switch (variant) {
    case 'simd':
      return new URL('./flashraw_simd.wasm', import.meta.url);
    case 'threads':
      return new URL('./flashraw_simd_threads.wasm', import.meta.url);
    case 'gpu':
      return new URL('./flashraw_simd_gpu.wasm', import.meta.url);
    default:
      return new URL('./flashraw.wasm', import.meta.url);
  }
}

function wrapWorker(w) {
  // Thread workers never keep the process alive on their own; the coordinator is ref'd while
  // a job is pending (`idle(false)`) so pending decodes do.
  // (Adding a 'message' listener re-refs the worker's port, so re-apply the state after it.)
  let busy = false;
  const apply = () => (busy ? w.ref() : w.unref());
  apply();
  let errorCb = null;
  return {
    post: (msg) => w.postMessage(msg),
    onMessage: (cb) => {
      w.on('message', cb);
      apply();
    },
    onError: (cb) => {
      errorCb = cb;
      w.on('error', cb);
      w.on('exit', (code) => errorCb?.(new Error(`thread worker exited with code ${code}`)));
    },
    idle: (isIdle) => {
      busy = !isIdle;
      apply();
    },
    terminate: () => {
      errorCb = null;
      return w.terminate();
    },
  };
}

_setPlatform({
  name: 'node',
  load: (variant, options) => readFile(wasmUrl(variant, options)),
  glue: (variant) => (variant === 'threads' ? import('./flashraw_bindgen_threads.js') : import('./flashraw_bindgen_gpu.js')),
  defaultSimd() {
    const v = process.env.FLASHRAW_WASM_SIMD;
    if (v === '0' || v === 'false') return false;
    return undefined;
  },
  // FLASHRAW_WASM_THREADS=0 disables the threaded build; =N uses N threads.
  defaultThreads() {
    const v = process.env.FLASHRAW_WASM_THREADS;
    if (v === undefined || v === '' || v === 'auto') return undefined;
    if (v === '0' || v === 'false' || v === 'off') return false;
    const n = Number(v);
    return Number.isInteger(n) && n > 0 ? n : undefined;
  },
  threadSupport() {
    try {
      new WebAssembly.Memory({ initial: 1, maximum: 1, shared: true });
      return null;
    } catch {
      return 'shared WebAssembly memory is not available';
    }
  },
  spawnThread: (name, options) =>
    wrapWorker(new Worker(options.threadWorkerUrl ?? new URL('./thread-worker.node.js', import.meta.url), { name })),
  // Files are read in chunks into WASM memory by core.js (`fileIn`): one copy, bounded JS
  // memory, and the event loop stays responsive.
  async fileSource(p) {
    const path = toPath(p);
    let fh;
    try {
      fh = await open(path, 'r');
    } catch (e) {
      throw ioError(e, path);
    }
    let size;
    try {
      size = (await fh.stat()).size;
    } catch (e) {
      await fh.close();
      throw ioError(e, path);
    }
    return {
      size,
      chunkSize: STAGING,
      /** Read into `view` from file offset `position`; resolves to the bytes read. */
      async read(view, position) {
        try {
          return (await fh.read(view, 0, view.length, position)).bytesRead;
        } catch (e) {
          throw ioError(e, path);
        }
      },
      close: () => fh.close(),
    };
  },
  async readHead(p, max) {
    const path = toPath(p);
    let fh;
    try {
      fh = await open(path, 'r');
      const { size } = await fh.stat();
      const head = Buffer.allocUnsafe(Math.min(size, max));
      const { bytesRead } = await fh.read(head, 0, head.length, 0);
      return { head: head.subarray(0, bytesRead), size };
    } catch (e) {
      throw ioError(e, path);
    } finally {
      await fh?.close();
    }
  },
  hardwareConcurrency: () => (availableParallelism ? availableParallelism() : cpus().length),
  workers: () => true,
});

export * from './core.js';
