#!/usr/bin/env node
// Benchmarks: WASM SIMD vs WASM scalar fallback vs native CLI (1 thread and all threads), in
// Node.js and (with --browser) headless Chromium. Same files, same outputs.
//
//   node scripts/bench.mjs [--browser] [--iterations N] [files...]
//
// Default files: the test fixtures plus a few larger corpus DNGs when $FLASHRAW_CORPUS (or the
// default corpus path) exists. Writes .build/bench.json and prints Markdown tables.

import { spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, writeFileSync } from 'node:fs';
import { basename, dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { homedir } from 'node:os';
import { fixtures, corpusFile, corpusRoot, cliBinary } from '../tests/helpers.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const pkgDir = resolve(here, '..');
const args = process.argv.slice(2);
const browser = args.includes('--browser');
const itIdx = args.indexOf('--iterations');
const iterations = itIdx >= 0 ? Number(args[itIdx + 1]) : 7;
const explicit = args.filter((a, i) => !a.startsWith('--') && args[i - 1] !== '--iterations');

const CORPUS = [
  'Leica/M8/L1030132.DNG',
  'Pentax/K-50/IMGP1385.DNG',
  'Apple/iPhone XS/IMG_1105.dng',
  'Google/Pixel 6 Pro/PXL_20220910_093206982.dng',
  'Sigma/fp/A002_647_20240229_000001_4k_12bit.DNG',
];
const files = explicit.length ? explicit.map((f) => resolve(f)) : [...fixtures, ...CORPUS.map(corpusFile).filter(Boolean)];

function nodeRun(simd) {
  const r = spawnSync(process.execPath, [join(here, 'bench-child.mjs'), ...files], {
    env: { ...process.env, FLASHRAW_WASM_SIMD: simd ? '' : '0', BENCH_ITERATIONS: String(iterations) },
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

async function chromiumRun(simd) {
  const { chromium } = await import('playwright');
  const { serve } = await import('./serve.mjs');
  const libs =
    process.env.FLASHRAW_BROWSER_LIBS ?? join(homedir(), '.cache/flashraw-browser-libs/root/usr/lib/x86_64-linux-gnu');
  const env = existsSync(libs) ? { ...process.env, LD_LIBRARY_PATH: libs } : process.env;
  const server = await serve({ extra: corpusRoot ? { '/corpus/': corpusRoot } : {} });
  const b = await chromium.launch({ env });
  try {
    const page = await b.newPage();
    const urls = files.map((f) =>
      f.startsWith(pkgDir) ? f.slice(pkgDir.length) : `/corpus/${f.slice(corpusRoot.length + 1).split('/').map(encodeURIComponent).join('/')}`,
    );
    const port = server.address().port;
    await page.goto(
      `http://127.0.0.1:${port}/scripts/bench-page.html?simd=${simd ? 1 : 0}&n=${iterations}&files=${urls.map(encodeURIComponent).join(',')}`,
    );
    await page.waitForFunction(() => window.__result, null, { timeout: 600_000 });
    const r = await page.evaluate(() => window.__result);
    if (r.error) throw new Error(r.error);
    return r;
  } finally {
    await b.close();
    server.close();
  }
}

const report = { date: new Date().toISOString(), iterations, node: process.version, files: files.map((f) => basename(f)) };
report.nodeSimd = nodeRun(true);
report.nodeFallback = nodeRun(false);
report.native1 = { 'raw-u16': native(1, 'raw-u16'), rgba8: native(1, 'rgba8') };
report.nativeAll = { 'raw-u16': native(0, 'raw-u16'), rgba8: native(0, 'rgba8') };
if (browser) {
  report.chromeSimd = await chromiumRun(true);
  report.chromeFallback = await chromiumRun(false);
}
mkdirSync(join(pkgDir, '.build'), { recursive: true });
writeFileSync(join(pkgDir, '.build/bench.json'), JSON.stringify(report, null, 2));

const fmt = (x) => (x === undefined || x === null ? 'n/a' : typeof x === 'number' ? x.toFixed(1) : x);
const cell = (run, name, output) => {
  const r = run?.results?.find((x) => x.file === name)?.[output];
  return r?.error ?? fmt(r?.wallMs);
};
const ncell = (n, name, output) => n?.[output]?.[name]?.error ?? fmt(n?.[output]?.[name]?.decodeMs);

for (const output of ['raw-u16', 'rgba8']) {
  console.log(`\n### ${output} (median ms; WASM = wall time incl. copy-out)\n`);
  const cols = ['File', 'MP', 'Node SIMD', 'Node scalar'];
  if (browser) cols.push('Chromium SIMD', 'Chromium scalar');
  cols.push('Native 1T', 'Native all cores');
  console.log(`| ${cols.join(' | ')} |\n|${cols.map(() => '---').join('|')}|`);
  for (const f of report.files) {
    const mp = report.nodeSimd.results.find((x) => x.file === f)?.megapixels;
    const row = [f, fmt(mp), cell(report.nodeSimd, f, output), cell(report.nodeFallback, f, output)];
    if (browser) row.push(cell(report.chromeSimd, f, output), cell(report.chromeFallback, f, output));
    row.push(ncell(report.native1, f, output), ncell(report.nativeAll, f, output));
    console.log(`| ${row.join(' | ')} |`);
  }
}
console.log(
  `\nInit (cold process, import + instantiate): Node SIMD ${fmt(report.nodeSimd.initMs)} ms (module ${fmt(report.nodeSimd.moduleInitMs)} ms), Node scalar ${fmt(report.nodeFallback.initMs)} ms` +
    (browser ? `, Chromium SIMD ${fmt(report.chromeSimd.initMs)} ms, Chromium scalar ${fmt(report.chromeFallback.initMs)} ms` : ''),
);
