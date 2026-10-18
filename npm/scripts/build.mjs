#!/usr/bin/env node
// Build the flashraw npm package:
//   cargo (wasm32, 6 variants) -> wasm-bindgen (--target web) -> wasm-opt -O3 -> dist/
//
// Variants (all shipped; the loader downloads one, plus a JPEG XL one only for JPEG XL DNGs):
//   simd      flashraw_simd.wasm          stable, +simd128                     (default single-threaded)
//   fallback  flashraw.wasm               stable, scalar                       (engines without SIMD)
//   threads   flashraw_simd_threads.wasm  nightly, +atomics,+bulk-memory,+mutable-globals,+simd128,
//                                         -Z build-std, shared memory, rayon  (default when isolated)
//   gpu       flashraw_simd_gpu.wasm      stable, +simd128, WebGPU backend      (opt-in: init({ gpu: true }))
//   simd-jxl     flashraw_simd_jxl.wasm          simd + JPEG XL DNG     (loaded when a file needs it)
//   threads-jxl  flashraw_simd_threads_jxl.wasm  threads + JPEG XL DNG  (idem, cross-origin isolated)
//
// Usage: node scripts/build.mjs [--no-opt] [--skip-cargo]
//                               [--variants simd,fallback,threads,gpu,simd-jxl,threads-jxl]
//
// Environment:
//   CARGO          cargo binary (default: `cargo` on PATH, then ~/.cargo/bin/cargo)
//   RUSTUP         rustup binary, used to run the nightly toolchain for the threaded variant
//   FLASHRAW_NIGHTLY  toolchain name for the threaded variant (default: nightly)
//   WASM_BINDGEN   wasm-bindgen binary; must match the wasm-bindgen version in Cargo.lock.
//                  When absent or mismatched, the matching CLI is installed into npm/.tools/.
//   FLASHRAW_NO_ICC=1  build without the `icc` feature (no arbitrary ICC output profiles, ~88 KiB smaller)
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
const varIdx = args.indexOf('--variants');
const only = varIdx >= 0 ? args[varIdx + 1].split(',') : null;

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
const rustup = process.env.RUSTUP || which('rustup') || join(homedir(), '.cargo/bin/rustup');
const nightly = process.env.FLASHRAW_NIGHTLY || 'nightly';

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
const THREAD_FLAGS = [
  '-C target-feature=+atomics,+bulk-memory,+mutable-globals,+simd128',
  // Shared, imported memory with a 4 GiB maximum (the default would cap it at 1 GiB), and the
  // TLS symbols wasm-bindgen needs to give each thread its own thread-local storage.
  '-C link-arg=--shared-memory',
  '-C link-arg=--import-memory',
  '-C link-arg=--max-memory=4294967296',
  '-C link-arg=--export=__wasm_init_tls',
  '-C link-arg=--export=__tls_size',
  '-C link-arg=--export=__tls_align',
  '-C link-arg=--export=__tls_base',
].join(' ');

const variants = [
  { name: 'simd', out: 'flashraw_simd.wasm', glue: 'flashraw_bindgen.js', rustflags: '-C target-feature=+simd128', features: '' },
  { name: 'fallback', out: 'flashraw.wasm', glue: 'flashraw_bindgen.js', rustflags: '', features: '' },
  {
    name: 'threads',
    out: 'flashraw_simd_threads.wasm',
    glue: 'flashraw_bindgen_threads.js',
    rustflags: THREAD_FLAGS,
    features: 'threads',
    nightly: true,
    opt: ['--enable-threads', '--enable-simd'],
  },
  { name: 'gpu', out: 'flashraw_simd_gpu.wasm', glue: 'flashraw_bindgen_gpu.js', rustflags: '-C target-feature=+simd128', features: 'gpu' },
  // JPEG XL variants: the simd / threads builds plus the `jpegxl` feature. Their glue must match
  // the base glue up to the initial memory size (asserted below; the thread workers reuse the
  // base threads glue with the memory they are given), but ships as its own file so both
  // instances can live side by side on the main thread.
  {
    name: 'simd-jxl',
    out: 'flashraw_simd_jxl.wasm',
    glue: 'flashraw_bindgen_jxl.js',
    sameGlueAs: 'flashraw_bindgen.js',
    rustflags: '-C target-feature=+simd128',
    features: 'jpegxl',
  },
  {
    name: 'threads-jxl',
    out: 'flashraw_simd_threads_jxl.wasm',
    glue: 'flashraw_bindgen_threads_jxl.js',
    sameGlueAs: 'flashraw_bindgen_threads.js',
    rustflags: THREAD_FLAGS,
    features: 'threads,jpegxl',
    nightly: true,
    opt: ['--enable-threads', '--enable-simd'],
  },
].filter((v) => !only || only.includes(v.name));

const bindgen = findBindgen(lockedVersion('wasm-bindgen'));
const wasmOpt = noOpt ? null : findWasmOpt();
if (!noOpt && !wasmOpt) log('warning: wasm-opt not found; shipping unoptimised wasm-bindgen output');

const tmp = join(pkgDir, '.build');
rmSync(tmp, { recursive: true, force: true });
mkdirSync(tmp, { recursive: true });
// A partial build (--variants) refreshes only its variants and the hand-written files.
if (!only) rmSync(dist, { recursive: true, force: true });
mkdirSync(dist, { recursive: true });

const glue = {};
const sameGlue = [];
for (const v of variants) {
  const targetDir = join(root, 'target', `wasm-${v.name}`);
  if (!skipCargo) {
    const cargoArgs = ['build', '-p', 'flashraw-wasm', '--target', 'wasm32-unknown-unknown', '--profile', 'wasm-release'];
    if (process.env.FLASHRAW_NO_ICC) cargoArgs.push('--no-default-features');
    if (v.features) cargoArgs.push('--features', v.features);
    cargoArgs.push('--target-dir', targetDir);
    // Each variant has its own target dir so switching RUSTFLAGS does not force full rebuilds.
    const env = { ...process.env, RUSTFLAGS: v.rustflags };
    if (v.nightly) {
      // std must be rebuilt with atomics enabled (needs the nightly rust-src component).
      run(rustup, ['run', nightly, 'cargo', ...cargoArgs, '-Z', 'build-std=panic_abort,std'], { env });
    } else {
      run(cargo, cargoArgs, { env });
    }
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
    const feat = v.opt ?? (v.rustflags.includes('simd128') ? ['--enable-simd'] : []);
    run(wasmOpt, [
      bg, '-O3', '--enable-bulk-memory', '--enable-nontrapping-float-to-int', '--enable-sign-ext',
      '--enable-mutable-globals', '--enable-reference-types', '--enable-multivalue', ...feat, '-o', dst,
    ]);
  } else {
    copyFileSync(bg, dst);
  }
  const js = readFileSync(join(outDir, 'flashraw_bindgen.js'), 'utf8');
  // Variants sharing a glue file must export the identical wasm-bindgen ABI; refuse to ship
  // if they ever diverge.
  if (glue[v.glue] !== undefined && glue[v.glue] !== js) {
    throw new Error(`wasm-bindgen glue differs between variants sharing ${v.glue}`);
  }
  glue[v.glue] = js;
  if (v.sameGlueAs) sameGlue.push([v.glue, v.sameGlueAs]);
}
// The default memory a glue creates when none is passed (`initial` pages) depends on the binary.
const abi = (js) => js.replace(/new WebAssembly\.Memory\(\{initial:\d+,/g, 'new WebAssembly.Memory({initial:N,');
for (const [a, b] of sameGlue) {
  const other = glue[b] ?? (existsSync(join(dist, b)) ? readFileSync(join(dist, b), 'utf8') : undefined);
  if (other !== undefined && abi(other) !== abi(glue[a])) throw new Error(`wasm-bindgen glue of ${a} differs from ${b}`);
}
for (const [file, js] of Object.entries(glue)) writeFileSync(join(dist, file), js);

// Hand-written JS + types.
const src = join(pkgDir, 'src');
for (const f of readdirSync(src)) {
  if (f.endsWith('.js') || f.endsWith('.d.ts')) copyFileSync(join(src, f), join(dist, f));
}

// Size report (also consumed by docs/wasm.md and scripts/bench.mjs).
const sizes = {};
for (const v of variants) {
  const b = readFileSync(join(dist, v.out));
  const g = Buffer.from(glue[v.glue]);
  sizes[v.name] = {
    file: v.out,
    raw: b.length,
    gzip: gzipSync(b, { level: 9 }).length,
    brotli: brotliCompressSync(b, { params: { [zc.BROTLI_PARAM_QUALITY]: 11 } }).length,
    glue: v.glue,
    glueRaw: g.length,
    glueGzip: gzipSync(g, { level: 9 }).length,
  };
}
const jsBytes = readdirSync(dist)
  .filter((f) => f.endsWith('.js'))
  .reduce((n, f) => n + statSync(join(dist, f)).size, 0);
const report = { wasmBindgen: lockedVersion('wasm-bindgen'), optimized: !!wasmOpt, sizes, jsBytes };
writeFileSync(join(tmp, 'sizes.json'), JSON.stringify(report, null, 2));
for (const v of variants) rmSync(join(tmp, v.name), { recursive: true, force: true });
const kb = (n) => `${(n / 1024).toFixed(1)} KiB`;
for (const [k, s] of Object.entries(sizes)) {
  log(
    `${k.padEnd(11)} ${s.file.padEnd(31)} raw ${kb(s.raw).padStart(11)}  gzip ${kb(s.gzip).padStart(10)}  brotli ${kb(s.brotli).padStart(10)}  (glue ${kb(s.glueRaw)})`,
  );
}
log(`JS (glue + wrappers, unminified): ${kb(jsBytes)}`);
log('done ->', dist);
