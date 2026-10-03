// worker_threads entry point used by `flashraw/worker` (Node.js).
import { parentPort } from 'node:worker_threads';
import * as api from './node.js';
import { serve } from './worker-protocol.js';

// Pool workers already run in parallel: no nested thread pools unless `init: { threads }` asks.
api._setDefaults({ threads: false });

serve(api, {
  post: (msg, transfer) => parentPort.postMessage(msg, transfer),
  listen: (cb) => parentPort.on('message', cb),
});
