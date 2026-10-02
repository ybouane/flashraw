// worker_threads entry point used by `flashraw/worker` (Node.js).
import { parentPort } from 'node:worker_threads';
import * as api from './node.js';
import { serve } from './worker-protocol.js';

serve(api, {
  post: (msg, transfer) => parentPort.postMessage(msg, transfer),
  listen: (cb) => parentPort.on('message', cb),
});
