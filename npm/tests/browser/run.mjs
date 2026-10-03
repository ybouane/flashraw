#!/usr/bin/env node
// Headless Chromium test (Playwright): loads dist/ in a real browser, decodes DNG fixtures to
// raw-u16 (hash vs native) and RGBA8 (ImageData on a canvas), and checks binary selection:
//   - cross-origin isolated page (COOP/COEP)  -> multithreaded build, output == single-threaded
//   - same page without isolation             -> single-threaded SIMD build, with the reason
//   - ?simd=0                                 -> scalar fallback build agrees
//   - init({ gpu: true })                     -> WebGPU build: GPU decode/resident outputs when
//                                                an adapter exists, clean CPU fallback otherwise
// plus the worker pool, GPU fallback of the default build, and structured errors.
//
//   node tests/browser/run.mjs          (after `npx playwright install chromium`)
//
// Environment: FLASHRAW_CHROME=<path> runs an installed Chrome instead of Playwright's
// Chromium (e.g. for real WebGPU); FLASHRAW_CHROME_ARGS adds command-line flags (space separated).

import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';
import { chromium } from 'playwright';
import { serve } from '../../scripts/serve.mjs';

const expected = JSON.parse(readFileSync(new URL('../fixtures/expected.json', import.meta.url), 'utf8'));
const FILES = ['canon-5d3-mlv.dng', 'bmpcc-tiles.dng', 'canon-a410-chdk.dng'];

// Hosts without Chromium's system libraries: see scripts/fetch-browser-libs.sh (no root needed).
const libs =
  process.env.FLASHRAW_BROWSER_LIBS ?? join(homedir(), '.cache/flashraw-browser-libs/root/usr/lib/x86_64-linux-gnu');
const env = existsSync(libs)
  ? { ...process.env, LD_LIBRARY_PATH: [libs, process.env.LD_LIBRARY_PATH].filter(Boolean).join(':') }
  : process.env;

const isolated = await serve({ isolated: true });
const plain = await serve({ isolated: false });
const url = (server) => `http://127.0.0.1:${server.address().port}`;
const browser = await chromium.launch({
  env,
  executablePath: process.env.FLASHRAW_CHROME || undefined,
  args: (process.env.FLASHRAW_CHROME_ARGS ?? '').split(' ').filter(Boolean),
});
let failed = 0;

async function load(query, server = isolated) {
  const page = await browser.newPage();
  const errors = [];
  page.on('pageerror', (e) => errors.push(String(e)));
  await page.goto(`${url(server)}/tests/browser/page.html?${query}`);
  await page.waitForFunction(() => window.__result, null, { timeout: 180_000 });
  const r = await page.evaluate(() => window.__result);
  await page.close();
  if (r.error) throw new Error(r.error);
  assert.deepEqual(errors, []);
  return r;
}

async function check(name, fn) {
  try {
    await fn();
    console.log(`ok - ${name}`);
  } catch (e) {
    failed++;
    console.log(`not ok - ${name}\n  ${String(e.stack ?? e).split('\n').join('\n  ')}`);
  }
}

function checkFiles(r, label) {
  for (const name of FILES) {
    const f = r.files[name];
    const e = expected[name];
    assert.equal(f.supports, true);
    assert.equal(f.metadata.format, 'DNG');
    assert.equal(f.metadata.rawWidth, e.width);
    assert.equal(f.raw.width, e.width);
    assert.equal(f.raw.height, e.height);
    assert.equal(f.raw.hash, e.hash_full, `${name} raw-u16 hash vs native CLI (${label})`);
    assert.ok(f.raw.timings.totalMs > 0);
    assert.ok(f.rgba.isClamped);
    assert.equal(f.rgba.bytes, f.rgba.width * f.rgba.height * 4);
    assert.equal(f.rgba.center[3], 255);
    if (f.gpu) {
      assert.equal(f.gpu.kind, 'cpu');
      assert.match(f.gpu.fallbackReason, /gpu/i);
      assert.deepEqual(f.gpuError, { name: 'GpuUnavailableError', kind: 'GpuUnavailable' });
    }
    if (e.previews > 0) assert.ok(f.preview.bytes > 0);
    if (f.preview?.bitmap) assert.deepEqual(f.preview.bitmap, [f.preview.width, f.preview.height]);
  }
  assert.deepEqual(r.garbage, { name: 'UnsupportedFormatError', kind: 'UnsupportedFormat', isFlashRawError: true });
}

const results = {};
await check('cross-origin isolated page: multithreaded build, raw-u16 + RGBA8 + worker pool', async () => {
  const r = await load(`files=${FILES.join(',')}&worker=1`);
  results.threads = r;
  assert.equal(r.caps.threads.crossOriginIsolated, true);
  assert.equal(r.info.binary, 'threads', r.caps.threads.reason);
  assert.equal(r.info.variant, 'simd');
  assert.equal(r.caps.simd.level, 'WASM SIMD128');
  assert.equal(r.caps.platform, 'browser');
  assert.equal(r.caps.threads.wasmThreads, true);
  assert.ok(r.caps.threads.count >= 1);
  checkFiles(r, 'threads');
  for (const name of FILES) {
    assert.match(r.files[name].raw.backend.decode, /WASM SIMD128 x\d+ thread/);
    assert.equal(r.files[name].raw.backend.threads, r.caps.threads.count);
  }
  const h = expected[FILES[0]].hash_full;
  assert.deepEqual([r.worker.a, r.worker.b], [h, h], 'worker pool output');
  assert.equal(r.worker.caps.platform, 'worker');
  assert.equal(r.worker.caps.wasm.binary, 'simd', 'pool workers default to the single-threaded build');
});

await check('single-threaded SIMD build (threads: false) agrees with the threaded build', async () => {
  const r = await load(`files=${FILES.join(',')}&threads=0`);
  results.simd = r;
  assert.equal(r.info.binary, 'simd');
  assert.equal(r.caps.threads.count, 0);
  assert.equal(r.caps.threads.reason, 'disabled by option');
  checkFiles(r, 'simd');
  for (const name of FILES) {
    assert.match(r.files[name].raw.backend.decode, /WASM SIMD128 x1 thread$/);
    if (results.threads) assert.deepEqual(r.files[name].rgba.center, results.threads.files[name].rgba.center);
  }
});

await check('page without cross-origin isolation falls back to the single-threaded build', async () => {
  const r = await load(`files=${FILES[0]}`, plain);
  assert.equal(r.caps.threads.crossOriginIsolated, false);
  assert.equal(r.info.binary, 'simd');
  assert.equal(r.caps.threads.wasmThreads, false);
  assert.equal(r.caps.threads.count, 0);
  assert.match(r.caps.threads.reason, /cross-origin isolated/);
  assert.equal(r.files[FILES[0]].raw.hash, expected[FILES[0]].hash_full);
});

await check('fallback (non-SIMD) build agrees', async () => {
  const r = await load(`files=${FILES.join(',')}&simd=0`);
  assert.equal(r.info.variant, 'fallback');
  assert.equal(r.info.binary, 'fallback');
  assert.equal(r.caps.simd.active, false);
  assert.match(r.caps.threads.reason, /requires WebAssembly SIMD/);
  for (const name of FILES) {
    assert.equal(r.files[name].raw.hash, expected[name].hash_full);
    assert.match(r.files[name].raw.backend.decode, /scalar/);
    if (results.simd) assert.deepEqual(r.files[name].rgba.center, results.simd.files[name].rgba.center);
  }
  results.fallback = r;
});

await check('WebGPU build: GPU decode when an adapter exists, CPU fallback with reason otherwise', async () => {
  // canon-a410-chdk.dng is bit-packed: the GPU path unpacks it straight from the file bytes.
  const r = await load(`files=canon-a410-chdk.dng&gpu=1`);
  results.gpu = r;
  assert.equal(r.info.binary, 'gpu');
  assert.equal(r.caps.gpu.compiled, true);
  assert.equal(r.files['canon-a410-chdk.dng'].raw.hash, expected['canon-a410-chdk.dng'].hash_full);
  const g = r.gpu;
  if (!r.caps.gpu.available) {
    console.log(`# WebGPU unavailable here (${r.caps.gpu.reason}); checked the CPU fallback`);
    assert.equal(g.device, false);
    assert.equal(g.rgba.kind, 'cpu');
    assert.ok(g.rgba.fallbackReason);
    assert.equal(g.rgba.diff.count, 0);
    assert.deepEqual(g.resident, { gpu: false, kind: 'cpu', output: 'rgba8', fallbackReason: g.rgba.fallbackReason });
    assert.deepEqual(g.error, { name: 'GpuUnavailableError', kind: 'GpuUnavailable' });
    return;
  }
  console.log(`# WebGPU adapter: ${JSON.stringify(r.caps.gpu.adapter?.description)}`);
  assert.equal(g.device, true);
  assert.equal(g.rgba.kind, 'gpu');
  assert.equal(g.rgba.fallbackReason, null);
  // GPU develop matches the CPU reference up to float rounding (see docs/gpu.md).
  assert.ok(g.rgba.diff.max <= 2 && g.rgba.diff.fraction < 0.01, JSON.stringify(g.rgba.diff));
  assert.equal(g.resident.gpu, true);
  assert.equal(g.resident.output, 'gpu-rgba8');
  assert.equal(g.resident.format, 'rgba8');
  assert.equal(g.resident.sameDevice, true);
  assert.equal(g.resident.byteLength, g.rgba.size[0] * g.rgba.size[1] * 4);
  assert.ok(g.resident.bufferSize >= g.resident.byteLength);
  assert.deepEqual(g.resident.texture, [...g.rgba.size, 'rgba8unorm']);
  assert.equal(g.resident.downloaded, 'rgba8');
  assert.ok(g.resident.diff.max <= 2 && g.resident.diff.fraction < 0.01, JSON.stringify(g.resident.diff));
  assert.equal(g.resident.detached, true);
  assert.equal(g.error, null);
});

await browser.close();
isolated.close();
plain.close();

const row = (r, n) => `${r.files[n].raw.timings.totalMs.toFixed(1)} ms raw-u16, ${r.files[n].rgba.timings.totalMs.toFixed(1)} ms rgba8`;
for (const [k, r] of Object.entries(results)) {
  if (r.initMs !== undefined) console.log(`# init ${k}: ${r.initMs.toFixed(1)} ms`);
}
for (const n of FILES) {
  const parts = ['threads', 'simd', 'fallback'].filter((k) => results[k]?.files[n]).map((k) => `${k} ${row(results[k], n)}`);
  console.log(`# ${n}: ${parts.join(' | ')}`);
}
if (failed) {
  console.log(`${failed} browser check(s) failed`);
  process.exit(1);
}
console.log('all browser checks passed');
