#!/usr/bin/env node
// Bundler smoke tests: install the packed tarball (exactly what npm publishes) into scratch
// apps, bundle with Vite (Rolldown) and esbuild, serve the output and run it in headless
// Chromium. Checks that the .wasm assets (and the worker, for Vite) are emitted and that the
// decoded output matches the native hash. The JPEG XL variants must be separate assets that are
// fetched only once a JPEG XL DNG is decoded (with a sample from the corpus /
// $FLASHRAW_REALWORLD when present).
//
//   node tests/bundlers/run.mjs

import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { cpSync, existsSync, mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright';
import { serve } from '../../scripts/serve.mjs';
import { corpusFile, realworldFile, fnv1a64U16 } from '../helpers.mjs';

const here = fileURLToPath(new URL('.', import.meta.url));
const pkgDir = resolve(here, '../..');
const work = join(pkgDir, '.build/bundlers');
const expected = JSON.parse(readFileSync(join(pkgDir, 'tests/fixtures/expected.json'), 'utf8'))['canon-5d3-mlv.dng'];
// A JPEG XL DNG (optional) and its hash from the Node build.
const jxlSample = realworldFile('pixls/54754/my_jxl_test.dng') ?? corpusFile('samsung/SM-S901E/20250305_173647.dng');
const jxlHash = jxlSample ? fnv1a64U16((await (await import('../../dist/node.js')).decodeRaw(jxlSample)).data) : null;
if (!jxlSample) console.log('# no JPEG XL sample: the on-demand JPEG XL decode is not exercised');

rmSync(work, { recursive: true, force: true });
mkdirSync(work, { recursive: true });
const tgz = execFileSync('npm', ['pack', '--silent', '--pack-destination', work], { cwd: pkgDir, encoding: 'utf8' })
  .trim()
  .split('\n')
  .pop();
const tarball = join(work, tgz);

function app(name) {
  const dir = join(work, name);
  mkdirSync(join(dir, 'public'), { recursive: true });
  writeFileSync(join(dir, 'package.json'), JSON.stringify({ name: `app-${name}`, private: true, type: 'module' }));
  execFileSync('npm', ['install', '--silent', '--no-audit', '--no-fund', tarball], { cwd: dir });
  cpSync(join(here, 'app/main.js'), join(dir, 'main.js'));
  cpSync(join(pkgDir, 'tests/fixtures/canon-5d3-mlv.dng'), join(dir, 'public/fixture.dng'));
  if (jxlSample) cpSync(jxlSample, join(dir, 'public/jxl.dng'));
  return dir;
}

const html = (script, withWorker) =>
  `<!doctype html><meta charset="utf-8"><script>window.__withWorker=${withWorker}</script><script type="module" src="${script}"></script>`;

const builds = {};

// --- Vite -------------------------------------------------------------------------------
{
  const dir = app('vite');
  writeFileSync(join(dir, 'index.html'), html('./main.js', true));
  const { build } = await import('vite');
  await build({ root: dir, logLevel: 'warn', build: { outDir: join(dir, 'dist'), emptyOutDir: true } });
  const assets = readdirSync(join(dir, 'dist/assets'));
  assert.ok(assets.some((f) => /^flashraw_simd-.*\.wasm$/.test(f)), `vite emitted the SIMD wasm (${assets})`);
  assert.ok(assets.some((f) => /^flashraw-.*\.wasm$/.test(f)), 'vite emitted the fallback wasm');
  assert.ok(assets.some((f) => /worker-entry/.test(f)), 'vite emitted the worker chunk');
  assert.ok(assets.some((f) => /^flashraw_simd_threads-.*\.wasm$/.test(f)), 'vite emitted the threaded wasm');
  assert.ok(assets.some((f) => /^flashraw_simd_gpu-.*\.wasm$/.test(f)), 'vite emitted the WebGPU wasm');
  assert.ok(assets.some((f) => /thread-worker/.test(f)), 'vite emitted the thread worker chunk');
  assert.ok(assets.some((f) => /^flashraw_simd_jxl-.*\.wasm$/.test(f)), 'vite emitted the JPEG XL wasm');
  assert.ok(assets.some((f) => /^flashraw_simd_threads_jxl-.*\.wasm$/.test(f)), 'vite emitted the threaded JPEG XL wasm');
  assert.ok(assets.some((f) => /^flashraw_bindgen_jxl-.*\.js$/.test(f)), 'vite split the JPEG XL glue into its own chunk');
  // Served cross-origin isolated: the threaded build must work from the bundle.
  builds.vite = { dir: join(dir, 'dist'), withWorker: true, isolated: true };
  console.log('ok - vite build');
}

// --- esbuild ------------------------------------------------------------------------------
{
  const dir = app('esbuild');
  const out = join(dir, 'dist');
  const esbuild = await import('esbuild');
  await esbuild.build({
    entryPoints: [join(dir, 'main.js')],
    bundle: true,
    format: 'esm',
    platform: 'browser',
    outfile: join(out, 'app.js'),
    logLevel: 'warning',
  });
  // esbuild keeps `new URL('./x.wasm', import.meta.url)` as-is: ship the .wasm next to the bundle.
  for (const f of ['flashraw_simd.wasm', 'flashraw.wasm', 'flashraw_simd_jxl.wasm']) {
    cpSync(join(dir, 'node_modules/flashraw/dist', f), join(out, f));
  }
  cpSync(join(dir, 'public/fixture.dng'), join(out, 'fixture.dng'));
  if (jxlSample) cpSync(jxlSample, join(out, 'jxl.dng'));
  writeFileSync(join(out, 'index.html'), html('./app.js', false));
  builds.esbuild = { dir: out, withWorker: false };
  console.log('ok - esbuild build');

  // Node-platform bundle too: wasm read with fs from next to the bundle.
  const nodeOut = join(dir, 'dist-node');
  writeFileSync(
    join(dir, 'node-main.js'),
    `import { readMetadata, decodeRaw } from 'flashraw';
     const m = await readMetadata(process.argv[2]);
     const f = await decodeRaw(process.argv[2]);
     const out = { format: m.format, width: f.width, height: f.height };
     if (process.argv[3]) out.jxl = (await decodeRaw(process.argv[3])).backend.binary;
     console.log(JSON.stringify(out));`,
  );
  await esbuild.build({
    entryPoints: [join(dir, 'node-main.js')],
    bundle: true,
    format: 'esm',
    platform: 'node',
    outfile: join(nodeOut, 'app.mjs'),
    logLevel: 'warning',
  });
  for (const f of ['flashraw_simd.wasm', 'flashraw.wasm', 'flashraw_simd_jxl.wasm']) {
    cpSync(join(dir, 'node_modules/flashraw/dist', f), join(nodeOut, f));
  }
  const argv = [join(nodeOut, 'app.mjs'), join(dir, 'public/fixture.dng'), ...(jxlSample ? [jxlSample] : [])];
  const r = JSON.parse(execFileSync(process.execPath, argv, { encoding: 'utf8' }));
  assert.deepEqual(r, { format: 'DNG', width: expected.width, height: expected.height, ...(jxlSample ? { jxl: 'simd-jxl' } : {}) });
  console.log('ok - esbuild node bundle');
}

// --- webpack 5 ----------------------------------------------------------------------------
{
  const dir = app('webpack');
  const out = join(dir, 'dist');
  const { default: webpack } = await import('webpack');
  const stats = await new Promise((ok, fail) =>
    webpack(
      {
        mode: 'production',
        context: dir,
        entry: './main.js',
        // Named assets, so the test can tell which .wasm the page fetched.
        output: { path: out, filename: 'app.js', publicPath: 'auto', assetModuleFilename: '[name]-[hash][ext]' },
        resolve: { modules: [join(dir, 'node_modules')] },
        performance: { hints: false },
      },
      (err, s) => (err ? fail(err) : ok(s)),
    ),
  );
  assert.ok(!stats.hasErrors(), stats.toString('errors-only'));
  const files = readdirSync(out);
  assert.equal(files.filter((f) => f.endsWith('.wasm')).length, 6, `webpack emitted all six wasm assets (${files})`);
  assert.ok(files.some((f) => /^flashraw_simd_threads_jxl-.*\.wasm$/.test(f)), 'webpack emitted the threaded JPEG XL wasm');
  cpSync(join(dir, 'public/fixture.dng'), join(out, 'fixture.dng'));
  if (jxlSample) cpSync(jxlSample, join(out, 'jxl.dng'));
  writeFileSync(join(out, 'index.html'), html('./app.js', true));
  builds.webpack = { dir: out, withWorker: true, isolated: true };
  console.log('ok - webpack build');
}

// --- run in Chromium -----------------------------------------------------------------------
const libs = process.env.FLASHRAW_BROWSER_LIBS ?? join(homedir(), '.cache/flashraw-browser-libs/root/usr/lib/x86_64-linux-gnu');
const browser = await chromium.launch({ env: existsSync(libs) ? { ...process.env, LD_LIBRARY_PATH: libs } : process.env });
let failed = 0;
for (const [name, b] of Object.entries(builds)) {
  const server = await serve({ root: b.dir, isolated: !!b.isolated });
  try {
    const page = await browser.newPage();
    await page.goto(`http://127.0.0.1:${server.address().port}/index.html`);
    await page.waitForFunction(() => window.__result, null, { timeout: 120_000 });
    const r = await page.evaluate(() => window.__result);
    assert.equal(r.error, undefined, r.error);
    assert.equal(r.width, expected.width);
    assert.equal(r.height, expected.height);
    assert.equal(r.hash, expected.hash_full);
    assert.equal(r.simd, true);
    assert.equal(r.rgba[2], r.rgba[0] * r.rgba[1] * 4);
    if (b.isolated) {
      assert.equal(r.binary, 'threads', `threaded build from the bundle (${r.threadsReason})`);
      assert.ok(r.threads >= 1);
      assert.match(r.backend, /thread/);
    } else {
      assert.equal(r.binary, 'simd');
      assert.match(r.threadsReason, /cross-origin isolated/);
    }
    if (b.withWorker) assert.equal(r.workerHash, expected.hash_full);
    assert.deepEqual(r.jxlBefore, [], 'no JPEG XL wasm fetched before a JPEG XL file');
    if (jxlSample) {
      const want = b.isolated ? 'threads-jxl' : 'simd-jxl';
      assert.equal(r.jxl.binary, want);
      assert.equal(r.jxl.hash, jxlHash);
      assert.equal(r.jxl.fetched.length, 1, `fetched once: ${r.jxl.fetched}`);
      assert.match(r.jxl.fetched[0], b.isolated ? /^flashraw_simd_threads_jxl/ : /^flashraw_simd_jxl/);
    }
    console.log(
      `ok - ${name} bundle runs in Chromium${b.withWorker ? ' (incl. worker pool)' : ''}` +
        `${b.isolated ? `, cross-origin isolated: ${r.threads} threads` : ', not isolated: single-threaded'}`,
    );
  } catch (e) {
    failed++;
    console.log(`not ok - ${name}: ${e.stack ?? e}`);
  } finally {
    server.close();
  }
}
await browser.close();
if (failed) process.exit(1);
console.log('all bundler checks passed');
