#!/usr/bin/env node
// Benchmarks: WASM single-threaded (SIMD) vs WASM multithreaded vs scalar fallback vs WebGPU vs
// native CLI (1 thread and all threads), in Node.js and (with --browser) a headless Chromium /
// Chrome. Same files, same outputs.
//
//   node scripts/bench.mjs [--browser] [--gpu] [--iterations N] [--threads N] [--no-node]
//                          [--no-fallback] [--chrome <path>] [--chrome-args "<flags>"] [files...]
//
// --gpu adds a WebGPU run in the browser (rgba8 read back, and left resident on the GPU);
// --chrome runs an installed Chrome (e.g. with `--enable-unsafe-webgpu`) instead of Playwright's
// Chromium. Default files: the test fixtures plus a few larger corpus DNGs when $FLASHRAW_CORPUS
// (or the default corpus path) exists. Writes .build/bench.json and prints Markdown tables.

import { spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, writeFileSync } from 'node:fs';
import { basename, dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { homedir } from 'node:os';
import { fixtures, corpusFile, cliBinary } from '../tests/helpers.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const pkgDir = resolve(here, '..');
const args = process.argv.slice(2);
const flag = (k) => args.includes(k);
const opt = (k, d) => {
  const i = args.indexOf(k);
  return i >= 0 ? args[i + 1] : d;
};
const VALUED = ['--iterations', '--threads', '--chrome', '--chrome-args'];
const browser = flag('--browser');
const gpu = flag('--gpu');
const iterations = Number(opt('--iterations', 7));
const threadCount = opt('--threads') ? Number(opt('--threads')) : null;
const explicit = args.filter((a, i) => !a.startsWith('--') && !VALUED.includes(args[i - 1]));

const CORPUS = [
  'Leica/M8/L1030132.DNG',
  'Pentax/K-50/IMGP1385.DNG',
  'Apple/iPhone XS/IMG_1105.dng',
  'Google/Pixel 6 Pro/PXL_20220910_093206982.dng',
  'Sigma/fp/A002_647_20240229_000001_4k_12bit.DNG',
];
const files = explicit.length ? explicit.map((f) => resolve(f)) : [...fixtures, ...CORPUS.map(corpusFile).filter(Boolean)];

function nodeRun(env) {
  const r = spawnSync(process.execPath, [join(here, 'bench-child.mjs'), ...files], {
    env: { ...process.env, BENCH_ITERATIONS: String(iterations), ...env },
    encoding: 'utf8',
    maxBuffer: 1 << 26,
  });
  if (r.status !== 0) throw new Error(r.stderr);
  return JSON.parse(r.stdout);
}

function native(threads, output) {
  const bin = cliBinary();
  if (!bin) return null;
  const out = {};
  for (const f of files) {
    const a = ['benchmark', '--json', '--iterations', String(iterations), '--output', output, f];
    if (threads) a.push('--threads', String(threads));
    const r = spawnSync(bin, a, { encoding: 'utf8' });
    if (r.status !== 0) {
      out[basename(f)] = { error: (r.stdout + r.stderr).trim().split('\n').pop() };
      continue;
    }
    const j = JSON.parse(r.stdout.trim().split('\n').pop());
    out[basename(f)] = { decodeMs: j.total_ms_median ?? j.decode_ms_median };
  }
  return out;
}

async function browserRun(query) {
  const { chromium } = await import('playwright');
  const { serve } = await import('./serve.mjs');
  const libs =
    process.env.FLASHRAW_BROWSER_LIBS ?? join(homedir(), '.cache/flashraw-browser-libs/root/usr/lib/x86_64-linux-gnu');
  const env = existsSync(libs) ? { ...process.env, LD_LIBRARY_PATH: libs } : process.env;
  // Every file's directory is served under its own prefix; cross-origin isolated so that the
  // threaded build can be selected (single-threaded runs pass threads=0).
  const extra = {};
  const urls = files.map((f, i) => {
    extra[`/f${i}/`] = dirname(f);
    return `/f${i}/${encodeURIComponent(basename(f))}`;
  });
  const server = await serve({ extra, isolated: true });
  const b = await chromium.launch({
    env,
    executablePath: opt('--chrome') || undefined,
    args: (opt('--chrome-args') ?? '').split(' ').filter(Boolean),
  });
  try {
    const page = await b.newPage();
    const port = server.address().port;
    await page.goto(
      `http://127.0.0.1:${port}/scripts/bench-page.html?${query}&n=${iterations}&files=${urls.map(encodeURIComponent).join(',')}`,
    );
    await page.waitForFunction(() => window.__result, null, { timeout: 3_600_000, polling: 1000 });
    const r = await page.evaluate(() => window.__result);
    if (r.error) throw new Error(r.error);
    return r;
  } finally {
    await b.close();
    server.close();
  }
}

const report = { date: new Date().toISOString(), iterations, node: process.version, files: files.map((f) => basename(f)) };
const runs = []; // [key, label]
if (!flag('--no-node')) {
  report.nodeSimd = nodeRun({ FLASHRAW_WASM_THREADS: '0' });
  report.nodeThreads = nodeRun({ FLASHRAW_WASM_THREADS: threadCount ? String(threadCount) : '' });
  runs.push(['nodeSimd', 'Node 1T'], ['nodeThreads', `Node ${report.nodeThreads.threads}T`]);
  if (!flag('--no-fallback')) {
    report.nodeFallback = nodeRun({ FLASHRAW_WASM_SIMD: '0' });
    runs.push(['nodeFallback', 'Node scalar']);
  }
}
if (browser) {
  report.chromeSimd = await browserRun('threads=0');
  report.chromeThreads = await browserRun(threadCount ? `threads=${threadCount}` : '');
  runs.push(['chromeSimd', 'Browser 1T'], ['chromeThreads', `Browser ${report.chromeThreads.threads}T`]);
  if (!flag('--no-fallback')) {
    report.chromeFallback = await browserRun('simd=0&threads=0');
    runs.push(['chromeFallback', 'Browser scalar']);
  }
  if (gpu) {
    report.chromeGpu = await browserRun('gpu=1&cases=raw-u16,gpu-rgba8-readback,gpu-rgba8-resident');
    report.userAgent = report.chromeGpu.userAgent;
    report.gpuAdapter = report.chromeGpu.gpu?.adapter ?? report.chromeGpu.gpu?.reason;
  }
}
report.native1 = { 'raw-u16': native(1, 'raw-u16'), rgba8: native(1, 'rgba8') };
report.nativeAll = { 'raw-u16': native(0, 'raw-u16'), rgba8: native(0, 'rgba8') };
mkdirSync(join(pkgDir, '.build'), { recursive: true });
writeFileSync(join(pkgDir, '.build/bench.json'), JSON.stringify(report, null, 2));

const fmt = (x) => (x === undefined || x === null ? 'n/a' : typeof x === 'number' ? x.toFixed(1) : x);
const cell = (run, name, key) => {
  const r = run?.results?.find((x) => x.file === name)?.[key];
  return r?.error ?? fmt(r?.wallMs);
};
const ncell = (n, name, output) => n?.[output]?.[name]?.error ?? fmt(n?.[output]?.[name]?.decodeMs);
const anyRun = report[runs[0]?.[0]] ?? report.chromeGpu;

for (const output of ['raw-u16', 'rgba8']) {
  console.log(`\n### ${output} (median ms; WASM = wall time incl. copy-out)\n`);
  const cols = ['File', 'MP', ...runs.map(([, l]) => l)];
  if (gpu && output === 'rgba8') cols.push('WebGPU readback', 'WebGPU resident');
  cols.push('Native 1T', 'Native all cores');
  console.log(`| ${cols.join(' | ')} |\n|${cols.map(() => '---').join('|')}|`);
  for (const f of report.files) {
    const mp = anyRun?.results.find((x) => x.file === f)?.megapixels;
    const row = [f, fmt(mp), ...runs.map(([k]) => cell(report[k], f, output))];
    if (gpu && output === 'rgba8') {
      row.push(cell(report.chromeGpu, f, 'gpu-rgba8-readback'), cell(report.chromeGpu, f, 'gpu-rgba8-resident'));
    }
    row.push(ncell(report.native1, f, output), ncell(report.nativeAll, f, output));
    console.log(`| ${row.join(' | ')} |`);
  }
}
console.log(`\nInit (cold, import + instantiate${browser ? ' + thread start' : ''}): ${runs.map(([k, l]) => `${l} ${fmt(report[k].initMs)} ms`).join(', ')}`);
if (gpu) console.log(`WebGPU: ${JSON.stringify(report.gpuAdapter)}; ${report.userAgent}`);
