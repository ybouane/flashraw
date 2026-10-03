// worker_threads entry of the multithreaded build: one FlashRaw thread (a rayon thread or the
// coordinator), sharing the main thread's WebAssembly.Memory. Started by `init()`; see threads.js.
import { parentPort } from 'node:worker_threads';
import * as glue from './flashraw_bindgen_threads.js';
import { runThreadWorker } from './threads.js';

runThreadWorker(glue, {
  post: (msg) => parentPort.postMessage(msg),
  listen: (cb) => parentPort.on('message', cb),
});
