// Browser / Web Worker / Deno entry point.
//
// The .wasm binaries and worker scripts are referenced with literal
// `new URL('./x', import.meta.url)` expressions so Vite, webpack 5, Rollup and Parcel emit them
// as assets automatically. With bundlers that do not (esbuild), copy `dist/*.wasm` (and, for
// the threaded build, `dist/thread-worker.browser.js` + its imports) next to your bundle or
// pass `init({ baseUrl })` / `init({ wasm })`.

import { _setPlatform } from './core.js';

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
  return {
    post: (msg) => w.postMessage(msg),
    onMessage: (cb) => w.addEventListener('message', (e) => cb(e.data)),
    onError: (cb) => {
      w.addEventListener('error', (e) => {
        e.preventDefault?.();
        cb(e.error ?? new Error(e.message || 'worker error (failed to load the thread worker script?)'));
      });
      w.addEventListener('messageerror', () => cb(new Error('worker message could not be deserialised')));
    },
    terminate: () => w.terminate(),
  };
}

_setPlatform({
  name: typeof window === 'undefined' && typeof self !== 'undefined' ? 'worker' : 'browser',
  // fetch() + instantiateStreaming compiles while downloading (served as application/wasm).
  load: (variant, options) => fetch(wasmUrl(variant, options)),
  glue: (variant) => (variant === 'threads' ? import('./flashraw_bindgen_threads.js') : import('./flashraw_bindgen_gpu.js')),
  // SharedArrayBuffer (and so shared WebAssembly.Memory) requires cross-origin isolation:
  // COOP `same-origin` + COEP `require-corp` (or `credentialless`) on the page.
  threadSupport() {
    if (typeof Worker !== 'function') return 'Web Workers are not available';
    if (globalThis.crossOriginIsolated !== true) {
      return 'the page is not cross-origin isolated, so SharedArrayBuffer is unavailable (serve it with COOP: same-origin and COEP: require-corp)';
    }
    if (typeof SharedArrayBuffer !== 'function') return 'SharedArrayBuffer is not available';
    return null;
  },
  // Literal worker options: bundlers only rewrite `new Worker(new URL(...), { static })`.
  spawnThread: (name, options) =>
    wrapWorker(
      options.threadWorkerUrl
        ? new Worker(options.threadWorkerUrl, { type: 'module', name })
        : new Worker(new URL('./thread-worker.browser.js', import.meta.url), { type: 'module', name: 'flashraw-thread' }),
    ),
  hardwareConcurrency: () => globalThis.navigator?.hardwareConcurrency ?? 1,
});

export * from './core.js';
