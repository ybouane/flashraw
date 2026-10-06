#!/usr/bin/env node
// prepack guard: refuse to pack without a complete build.
import { existsSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

const need = [
  'flashraw_bindgen.js',
  'flashraw_bindgen_threads.js',
  'flashraw_bindgen_gpu.js',
  'flashraw_bindgen_jxl.js',
  'flashraw_bindgen_threads_jxl.js',
  'flashraw_simd.wasm',
  'flashraw.wasm',
  'flashraw_simd_threads.wasm',
  'flashraw_simd_gpu.wasm',
  'flashraw_simd_jxl.wasm',
  'flashraw_simd_threads_jxl.wasm',
  'threads.js',
  'thread-worker.browser.js',
  'thread-worker.node.js',
  'core.js',
  'browser.js',
  'node.js',
  'errors.js',
  'worker-protocol.js',
  'worker.browser.js',
  'worker.node.js',
  'worker-entry.browser.js',
  'worker-entry.node.js',
  'index.d.ts',
  'worker.d.ts',
];
const missing = need.filter((f) => !existsSync(fileURLToPath(new URL(`../dist/${f}`, import.meta.url))));
if (missing.length) {
  console.error(`dist/ is incomplete (missing ${missing.join(', ')}); run \`npm run build\` first`);
  process.exit(1);
}
