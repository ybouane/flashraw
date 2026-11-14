// Web Worker entry point used by `flashraw/worker` (browser).
import * as api from './browser.js';
import { serve } from './worker-protocol.js';

// Pool workers already run in parallel: no nested thread pools unless `init: { threads }` asks.
api._setDefaults({ threads: false });

serve(api, {
  post: (msg, transfer) => self.postMessage(msg, transfer),
  listen: (cb) => self.addEventListener('message', (e) => cb(e.data)),
});
