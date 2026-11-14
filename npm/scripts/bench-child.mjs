// One benchmark run in a fresh Node process (so init latency is cold). FLASHRAW_WASM_SIMD=0
// selects the fallback binary, FLASHRAW_WASM_THREADS=0|N the single-threaded build / N threads.
// Prints JSON.
import { readFileSync } from 'node:fs';
import { basename } from 'node:path';

const t0 = performance.now();
const fr = await import('../dist/node.js');
const importMs = performance.now() - t0;
const info = await fr.init();
const initMs = performance.now() - t0;
const caps = await fr.getCapabilities();
const { benchFile } = await import('./bench-lib.mjs');
const iterations = Number(process.env.BENCH_ITERATIONS ?? 7);
const results = [];
for (const file of process.argv.slice(2)) {
  results.push(await benchFile(fr, basename(file), readFileSync(file), { iterations }));
}
process.stdout.write(
  JSON.stringify({ variant: info.variant, binary: info.binary, threads: caps.threads.count, importMs, initMs, moduleInitMs: info.initMs, results }),
);
