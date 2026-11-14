// Web Worker entry of the multithreaded build: one FlashRaw thread (a rayon thread or the
// coordinator), sharing the main thread's WebAssembly.Memory. Started by `init()`; see threads.js.
import * as glue from './flashraw_bindgen_threads.js';
import { runThreadWorker } from './threads.js';

runThreadWorker(glue, {
  post: (msg) => self.postMessage(msg),
  listen: (cb) => self.addEventListener('message', (e) => cb(e.data)),
});
