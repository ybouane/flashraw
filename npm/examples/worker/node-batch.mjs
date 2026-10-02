#!/usr/bin/env node
// Batch-decode RAW files with a worker_threads pool and compare with sequential decoding.
//
//   node examples/worker/node-batch.mjs [--workers 4] [--output rgba8] file1.dng file2.dng ...
//
// In your project: import { createWorkerPool } from 'flashraw/worker';
import { createWorkerPool } from '../../dist/worker.node.js';
import { decodeRaw } from '../../dist/node.js';

const args = process.argv.slice(2);
const opt = (k, d) => {
  const i = args.indexOf(k);
  return i >= 0 ? args[i + 1] : d;
};
const files = args.filter((a, i) => !a.startsWith('--') && !['--workers', '--output'].includes(args[i - 1]));
if (!files.length) {
  console.error('usage: node-batch.mjs [--workers N] [--output raw-u16|rgba8] <files...>');
  process.exit(2);
}
const output = opt('--output', 'raw-u16');

let t = performance.now();
for (const f of files) await decodeRaw(f, { output });
const sequential = performance.now() - t;

const pool = createWorkerPool({ size: Number(opt('--workers', 4)) });
await Promise.all(Array.from({ length: pool.size }, () => pool.getCapabilities())); // spawn + init workers
t = performance.now();
const results = await Promise.allSettled(files.map((f) => pool.decode(f, { output })));
const parallel = performance.now() - t;
pool.terminate();

results.forEach((r, i) => {
  const f = files[i];
  if (r.status === 'fulfilled') {
    console.log(`${f}: ${r.value.width}x${r.value.height} ${output} in ${r.value.timings.totalMs.toFixed(1)} ms`);
  } else {
    console.log(`${f}: ${r.reason.kind} ${r.reason.message}`);
  }
});
console.log(
  `\n${files.length} files: sequential ${sequential.toFixed(0)} ms, ${pool.size} workers ${parallel.toFixed(0)} ms ` +
    `(${(sequential / parallel).toFixed(2)}x)`,
);
