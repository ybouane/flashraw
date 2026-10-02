#!/usr/bin/env node
// Headless Chromium test (Playwright): loads dist/ in a real browser, decodes DNG fixtures to
// raw-u16 (hash vs native) and RGBA8 (ImageData on a canvas), checks the SIMD binary is used,
// the scalar fallback agrees, the worker pool works, and GPU requests fall back cleanly.
//
//   node tests/browser/run.mjs          (after `npx playwright install chromium`)

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

const server = await serve({ isolated: true });
const base = `http://127.0.0.1:${server.address().port}`;
const browser = await chromium.launch({ env });
let failed = 0;

async function load(query) {
  const page = await browser.newPage();
  const errors = [];
  page.on('pageerror', (e) => errors.push(String(e)));
  await page.goto(`${base}/tests/browser/page.html?${query}`);
  await page.waitForFunction(() => window.__result, null, { timeout: 120_000 });
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

const results = {};
await check('SIMD build: decode fixtures to raw-u16 + RGBA8', async () => {
  const r = await load(`files=${FILES.join(',')}&worker=1`);
  results.simd = r;
  assert.equal(r.info.variant, 'simd');
  assert.equal(r.caps.simd.active, true);
  assert.equal(r.caps.simd.level, 'WASM SIMD128');
  assert.equal(r.caps.platform, 'browser');
  assert.equal(r.caps.threads.crossOriginIsolated, true);
  for (const name of FILES) {
    const f = r.files[name];
    const e = expected[name];
    assert.equal(f.supports, true);
    assert.equal(f.metadata.format, 'DNG');
    assert.equal(f.metadata.rawWidth, e.width);
    assert.equal(f.raw.width, e.width);
    assert.equal(f.raw.height, e.height);
    assert.equal(f.raw.hash, e.hash_full, `${name} raw-u16 hash vs native CLI`);
    assert.match(f.raw.backend.decode, /WASM SIMD128/);
    assert.ok(f.raw.timings.totalMs > 0);
    assert.ok(f.rgba.isClamped);
    assert.equal(f.rgba.bytes, f.rgba.width * f.rgba.height * 4);
    assert.equal(f.rgba.center[3], 255);
    assert.equal(f.gpu.kind, 'cpu');
    assert.match(f.gpu.fallbackReason, /gpu/i);
    assert.deepEqual(f.gpuError, { name: 'GpuUnavailableError', kind: 'GpuUnavailable' });
    if (e.previews > 0) assert.ok(f.preview.bytes > 0);
    if (f.preview?.bitmap) assert.deepEqual(f.preview.bitmap, [f.preview.width, f.preview.height]);
  }
  assert.deepEqual(r.garbage, { name: 'UnsupportedFormatError', kind: 'UnsupportedFormat', isFlashRawError: true });
  const h = expected[FILES[0]].hash_full;
  assert.deepEqual([r.worker.a, r.worker.b], [h, h], 'worker pool output');
  assert.equal(r.worker.caps.platform, 'worker');
});

await check('fallback (non-SIMD) build agrees', async () => {
  const r = await load(`files=${FILES.join(',')}&simd=0`);
  assert.equal(r.info.variant, 'fallback');
  assert.equal(r.caps.simd.active, false);
  for (const name of FILES) {
    assert.equal(r.files[name].raw.hash, expected[name].hash_full);
    assert.match(r.files[name].raw.backend.decode, /scalar/);
    assert.deepEqual(r.files[name].rgba.center, results.simd.files[name].rgba.center);
  }
  results.fallback = r;
});

await browser.close();
server.close();

if (results.simd) {
  const row = (r, n) => `${r.files[n].raw.timings.totalMs.toFixed(1)} ms raw-u16, ${r.files[n].rgba.timings.totalMs.toFixed(1)} ms rgba8`;
  console.log(`# init ${results.simd.initMs.toFixed(1)} ms (SIMD)${results.fallback ? `, ${results.fallback.initMs.toFixed(1)} ms (fallback)` : ''}`);
  for (const n of FILES) {
    console.log(`# ${n}: SIMD ${row(results.simd, n)}${results.fallback ? ` | fallback ${row(results.fallback, n)}` : ''}`);
  }
}
if (failed) {
  console.log(`${failed} browser check(s) failed`);
  process.exit(1);
}
console.log('all browser checks passed');
