// `flashraw/worker` for Node.js: a pool of worker_threads, each with its own WASM instance.
//
//   import { createWorkerPool } from 'flashraw/worker';
//   const pool = createWorkerPool({ size: 4 });
//   const frames = await Promise.all(paths.map((p) => pool.decode(p))); // paths opened in workers
//   pool.terminate();
//
// Idle workers are unref()'d so an idle pool does not keep the process alive.

import { Worker } from 'node:worker_threads';
import { availableParallelism, cpus } from 'node:os';
import { WorkerPool, RemoteFlashRaw } from './worker-protocol.js';

export { WorkerPool, RemoteFlashRaw };

function spawnNode(options) {
  const w = options.createWorker
    ? options.createWorker()
    : new Worker(options.workerUrl ?? new URL('./worker-entry.node.js', import.meta.url), {
        env: process.env,
        name: 'flashraw',
      });
  w.unref();
  let errorCb = null;
  return {
    post: (msg, transfer) => w.postMessage(msg, transfer),
    onMessage: (cb) => w.on('message', cb),
    onError: (cb) => {
      errorCb = cb;
      w.on('error', cb);
      w.on('exit', (code) => errorCb?.(new Error(`worker exited with code ${code}`)));
    },
    idle: (isIdle) => (isIdle ? w.unref() : w.ref()),
    terminate: () => {
      errorCb = null;
      return w.terminate();
    },
  };
}

/**
 * Create a pool of FlashRaw worker threads.
 * @param {import('./worker.d.ts').WorkerPoolOptions} [options]
 */
export function createWorkerPool(options = {}) {
  const hc = availableParallelism ? availableParallelism() : cpus().length;
  return new WorkerPool(() => spawnNode(options), { hardwareConcurrency: hc, ...options });
}
