// Web Worker entry point used by `flashraw/worker` (browser).
import * as api from './browser.js';
import { serve } from './worker-protocol.js';

serve(api, {
  post: (msg, transfer) => self.postMessage(msg, transfer),
  listen: (cb) => self.addEventListener('message', (e) => cb(e.data)),
});
