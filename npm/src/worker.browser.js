// `flashraw/worker` for browsers: a pool of module Web Workers.
//
//   import { createWorkerPool } from 'flashraw/worker';
//   const pool = createWorkerPool({ size: 2 });
//   const frame = await pool.decode(file, { output: 'rgba8' }); // File/Blob read inside the worker
//
// The literal `new URL('./worker-entry.browser.js', import.meta.url)` lets Vite / webpack 5 /
// Rollup bundle the worker. Pass `workerUrl` (or `createWorker`) to host it yourself.

import { WorkerPool, RemoteFlashRaw } from './worker-protocol.js';
import { NotAvailableError } from './errors.js';

export { WorkerPool, RemoteFlashRaw };

function spawnBrowser(options) {
  if (typeof Worker !== 'function') throw new NotAvailableError('Web Workers are not available in this environment');
  const w = options.createWorker
    ? options.createWorker()
    : options.workerUrl
      ? new Worker(options.workerUrl, { type: 'module', name: 'flashraw' })
      : new Worker(new URL('./worker-entry.browser.js', import.meta.url), { type: 'module', name: 'flashraw' });
  return {
    post: (msg, transfer) => w.postMessage(msg, transfer),
    onMessage: (cb) => w.addEventListener('message', (e) => cb(e.data)),
    onError: (cb) => {
      w.addEventListener('error', (e) => {
        e.preventDefault?.();
        cb(e.error ?? new Error(e.message || 'worker error (failed to load the worker script?)'));
      });
      w.addEventListener('messageerror', () => cb(new Error('worker message could not be deserialised')));
    },
    terminate: () => w.terminate(),
  };
}

/**
 * Create a pool of FlashRaw workers.
 * @param {import('./worker.d.ts').WorkerPoolOptions} [options]
 */
export function createWorkerPool(options = {}) {
  return new WorkerPool(() => spawnBrowser(options), options);
}
