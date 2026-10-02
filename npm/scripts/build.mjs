#!/usr/bin/env node
// Build the flashraw npm package:
//   cargo (wasm32, SIMD128 + scalar fallback) -> wasm-bindgen (--target web) -> wasm-opt -O3 -> dist/
//
// Usage: node scripts/build.mjs [--no-opt] [--skip-cargo] [--features gpu]
//
// Environment:
//   CARGO          cargo binary (default: `cargo` on PATH, then ~/.cargo/bin/cargo)
//   WASM_BINDGEN   wasm-bindgen binary; must match the wasm-bindgen version in Cargo.lock.
//                  When absent or mismatched, the matching CLI is installed into npm/.tools/.
//   WASM_OPT       wasm-opt binary (default: binaryen from node_modules, then PATH)

import { execFileSync, spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync, copyFileSync, readdirSync, statSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { homedir } from 'node:os';
import { gzipSync, brotliCompressSync, constants as zc } from 'node:zlib';

const here = dirname(fileURLToPath(import.meta.url));
const pkgDir = resolve(here, '..');
const root = resolve(pkgDir, '..');
const dist = join(pkgDir, 'dist');
const args = process.argv.slice(2);
const noOpt = args.includes('--no-opt');
const skipCargo = args.includes('--skip-cargo');
const featIdx = args.indexOf('--features');
const features = featIdx >= 0 ? args[featIdx + 1] : '';

function log(...m) {
  console.log('[flashraw build]', ...m);
}

function which(cmd) {
  const r = spawnSync(process.platform === 'win32' ? 'where' : 'which', [cmd], { encoding: 'utf8' });
  return r.status === 0 ? r.stdout.split('\n')[0].trim() : null;
}

function run(cmd, argv, opts = {}) {
  log('$', cmd, argv.join(' '));
  execFileSync(cmd, argv, { stdio: 'inherit', cwd: root, ...opts });
}

const cargo = process.env.CARGO || which('cargo') || join(homedir(), '.cargo/bin/cargo');
if (!existsSync(cargo) && !which(cargo)) throw new Error(`cargo not found (set CARGO)`);

// --- wasm-bindgen version must match the crate version in Cargo.lock -----------------------
function lockedVersion(name) {
  const lock = readFileSync(join(root, 'Cargo.lock'), 'utf8');
  const m = lock.match(new RegExp(`name = "${name}"\\nversion = "([^"]+)"`));
  if (!m) throw new Error(`${name} not found in Cargo.lock`);
  return m[1];
}

function bindgenVersion(bin) {
  try {
    return execFileSync(bin, ['--version'], { encoding: 'utf8' }).trim().split(/\s+/)[1];
  } catch {
    return null;
  }
}

function findBindgen(want) {
  const local = join(pkgDir, '.tools/bin/wasm-bindgen');
  for (const c of [process.env.WASM_BINDGEN, local, which('wasm-bindgen'), join(homedir(), '.cargo/bin/wasm-bindgen')]) {
    if (c && bindgenVersion(c) === want) return c;
  }
  log(`installing wasm-bindgen-cli ${want} into npm/.tools (matches Cargo.lock)`);
  run(cargo, ['install', 'wasm-bindgen-cli', '--version', want, '--locked', '--root', join(pkgDir, '.tools')]);
  return local;
}

function findWasmOpt() {
  const local = join(pkgDir, 'node_modules/.bin/wasm-opt');
  for (const c of [process.env.WASM_OPT, local, which('wasm-opt')]) {
    if (c && existsSync(c)) return c;
  }
  return null;
}

// --- build -------------------------------------------------------------------------------
const variants = [
  { name: 'simd', out: 'flashraw_simd.wasm', rustflags: '-C target-feature=+simd128' },
  { name: 'fallback', out: 'flashraw.wasm', rustflags: '' },
];

const bindgen = findBindgen(lockedVersion('wasm-bindgen'));
const wasmOpt = noOpt ? null : findWasmOpt();
if (!noOpt && !wasmOpt) log('warning: wasm-opt not found; shipping unoptimised wasm-bindgen output');

const tmp = join(pkgDir, '.build');
rmSync(tmp, { recursive: true, force: true });
mkdirSync(tmp, { recursive: true });
rmSync(dist, { recursive: true, force: true });
mkdirSync(dist, { recursive: true });

const glue = {};
for (const v of variants) {
  const targetDir = join(root, 'target', `wasm-${v.name}`);
  if (!skipCargo) {
    const cargoArgs = ['build', '-p', 'flashraw-wasm', '--target', 'wasm32-unknown-unknown', '--profile', 'wasm-release'];
    if (features) cargoArgs.push('--features', features);
    cargoArgs.push('--target-dir', targetDir);
    // Each variant has its own target dir so switching RUSTFLAGS does not force full rebuilds.
    run(cargo, cargoArgs, { env: { ...process.env, RUSTFLAGS: v.rustflags } });
  }
  const wasmIn = join(targetDir, 'wasm32-unknown-unknown/wasm-release/flashraw_wasm.wasm');
  const outDir = join(tmp, v.name);
  run(bindgen, [
    wasmIn,
    '--out-dir', outDir,
    '--out-name', 'flashraw_bindgen',
    '--target', 'web',
    '--no-typescript',
    '--omit-default-module-path',
    '--remove-producers-section',
  ]);
  const bg = join(outDir, 'flashraw_bindgen_bg.wasm');
  const dst = join(dist, v.out);
  if (wasmOpt) {
    const feat = v.name === 'simd' ? ['--enable-simd'] : [];
    run(wasmOpt, [
      bg, '-O3', '--enable-bulk-memory', '--enable-nontrapping-float-to-int', '--enable-sign-ext',
      '--enable-mutable-globals', '--enable-reference-types', '--enable-multivalue', ...feat, '-o', dst,
    ]);
  } else {
    copyFileSync(bg, dst);
  }
  glue[v.name] = readFileSync(join(outDir, 'flashraw_bindgen.js'), 'utf8');
}

// Both binaries export the identical wasm-bindgen ABI, so one JS glue serves both; the loader
// picks which .wasm to instantiate. Refuse to ship if they ever diverge.
if (glue.simd !== glue.fallback) {
  throw new Error('wasm-bindgen glue differs between SIMD and fallback builds; cannot share one glue file');
}
writeFileSync(join(dist, 'flashraw_bindgen.js'), glue.simd);

// Hand-written JS + types.
const src = join(pkgDir, 'src');
for (const f of readdirSync(src)) {
  if (f.endsWith('.js') || f.endsWith('.d.ts')) copyFileSync(join(src, f), join(dist, f));
}

// Size report (also consumed by docs/wasm.md and scripts/bench.mjs).
const sizes = {};
for (const v of variants) {
  const b = readFileSync(join(dist, v.out));
  sizes[v.name] = {
    file: v.out,
    raw: b.length,
    gzip: gzipSync(b, { level: 9 }).length,
    brotli: brotliCompressSync(b, { params: { [zc.BROTLI_PARAM_QUALITY]: 11 } }).length,
  };
}
const jsBytes = readdirSync(dist)
  .filter((f) => f.endsWith('.js'))
  .reduce((n, f) => n + statSync(join(dist, f)).size, 0);
const report = { wasmBindgen: lockedVersion('wasm-bindgen'), optimized: !!wasmOpt, features, sizes, jsBytes };
writeFileSync(join(tmp, 'sizes.json'), JSON.stringify(report, null, 2));
rmSync(join(tmp, 'simd'), { recursive: true, force: true });
rmSync(join(tmp, 'fallback'), { recursive: true, force: true });
const kb = (n) => `${(n / 1024).toFixed(1)} KiB`;
for (const [k, s] of Object.entries(sizes)) {
  log(`${k.padEnd(8)} ${s.file.padEnd(20)} raw ${kb(s.raw).padStart(11)}  gzip ${kb(s.gzip).padStart(10)}  brotli ${kb(s.brotli).padStart(10)}`);
}
log(`JS (glue + wrappers, unminified): ${kb(jsBytes)}`);
log('done ->', dist);
