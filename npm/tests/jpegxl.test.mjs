// JPEG XL DNGs (compression 52546): the default binaries are built without the codec; the JPEG
// XL variants (flashraw_simd_jxl.wasm, flashraw_simd_threads_jxl.wasm) are loaded on demand
// (`init({ jpegxl: 'auto' })`), up front ('always') or never ('never').
//
// Samples: two corpus DNGs carrying compression 52546 (one in its raw IFD, one only in a
// secondary IFD, which must not load the variant) and real-world JPEG XL DNGs
// ($FLASHRAW_REALWORLD). Without them only the fixture-based checks run.
import { test, describe } from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { basename, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { FlashRaw, init, getCapabilities, _internals } from 'flashraw';
import { createWorkerPool } from 'flashraw/worker';
import { fixture, fixturesDir, corpusFile, realworldFile, fnv1a64U16, nativeVerify, cliBinary } from './helpers.mjs';

const script = fileURLToPath(new URL('./jpegxl-child.mjs', import.meta.url));
const expected = JSON.parse(readFileSync(`${fixturesDir}/expected.json`, 'utf8'));

// `exact`: lossless codestreams decode bit-identically everywhere. Lossy (VarDCT) ones are float
// math: the native x86-64 build runs the codec's AVX2/FMA kernels, WASM the portable ones, so a
// few samples may round to the neighbouring level.
const JXL = [
  { file: corpusFile('samsung/SM-S901E/20250305_173647.dng'), exact: true },
  { file: realworldFile('pixls/54754/my_jxl_test.dng'), exact: true },
  { file: realworldFile('pixls/54775/30YELfBfqJWkhvrznqs7R8aKx0X.dng'), exact: false },
].filter((s) => s.file);
const JXL_FILES = JXL.map((s) => s.file);
// Compression 52546 only outside the raw IFD: decodes with the small build.
const SECONDARY = corpusFile('Canon/EOS 7D Mark II/0P9A0410-Enhanced-NR.dng');
const PLAIN = [fixture('canon-5d3-mlv.dng'), ...(SECONDARY ? [SECONDARY] : [])];
const LAST = fixture('bmpcc-tiles.dng');
// The quickest JPEG XL sample (12 MP), for the checks that do not need all of them.
const QUICK = JXL_FILES.filter((f) => /my_jxl_test/.test(f)).concat(JXL_FILES).slice(0, 1);
const noJxl = JXL.length === 0 && 'no JPEG XL samples (corpus / $FLASHRAW_REALWORLD)';

function run(env, files) {
  const out = execFileSync(process.execPath, [script, ...files], {
    env: { ...process.env, FLASHRAW_WASM_SIMD: '', FLASHRAW_WASM_THREADS: '', FLASHRAW_WASM_JPEGXL: '', ...env },
    encoding: 'utf8',
    maxBuffer: 1 << 24,
  });
  return JSON.parse(out);
}

const byFile = (r) => Object.fromEntries(r.files.map((f) => [f.file, f]));

/** Per-sample comparison with the native CLI's raw-u16 output. */
function nativeDiff(file, data) {
  const dir = mkdtempSync(join(tmpdir(), 'flashraw-jxl-'));
  try {
    const out = join(dir, 'native.raw');
    execFileSync(cliBinary(), ['decode', file, out], { stdio: 'ignore' });
    const b = readFileSync(out);
    const n = new Uint16Array(b.buffer, b.byteOffset, b.length >> 1);
    assert.equal(n.length, data.length);
    let max = 0;
    let count = 0;
    for (let i = 0; i < n.length; i++) {
      const d = Math.abs(n[i] - data[i]);
      if (d) {
        count++;
        if (d > max) max = d;
      }
    }
    return { max, count, fraction: count / n.length };
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

// Reference hashes of the JPEG XL samples, filled by the in-process test (native-checked).
const reference = {};

describe('JPEG XL DNGs in this process (auto mode)', () => {
  test('init rejects an unknown jpegxl mode', async () => {
    await assert.rejects(init({ jpegxl: 'sometimes' }), { kind: 'InvalidArgument', message: /auto.*always.*never/ });
  });

  test('non-JPEG-XL files never load the variant', async () => {
    await init();
    for (const file of PLAIN) {
      const raw = await FlashRaw.openFile(file);
      const f = await raw.decode();
      assert.equal(f.backend.jpegxl, false);
      assert.equal(f.backend.binary, raw.binary);
      if (expected[basename(file)]) assert.equal(fnv1a64U16(f.data), expected[basename(file)].hash_full);
      raw.close();
    }
    assert.equal(_internals().jpegxl.loads, 0);
    const caps = await getCapabilities();
    assert.equal(caps.jpegxl.mode, 'auto');
    assert.equal(caps.jpegxl.loaded, false);
    assert.equal(caps.jpegxl.available, true);
  });

  test('JPEG XL DNGs decode like the native CLI (built with --features jpegxl)', { skip: noJxl }, async () => {
    const small = _internals().build.binary;
    for (const { file, exact } of JXL) {
      const raw = await FlashRaw.openFile(file);
      assert.equal(raw.frameInfo.compression, 'jpeg-xl');
      assert.equal(raw.binary, small, 'opened with the small build');
      const f = await raw.decode();
      assert.equal(raw.binary, `${small}-jxl`);
      assert.deepEqual([f.backend.binary, f.backend.jpegxl], [`${small}-jxl`, true]);
      const hash = fnv1a64U16(f.data);
      reference[file] = hash;
      const ref = nativeVerify(file);
      if (ref && !ref.error_kind) {
        assert.equal(f.width, ref.raw_width);
        assert.equal(f.height, ref.raw_height);
        if (exact) assert.equal(hash, ref.hash_full, `${basename(file)} vs native CLI`);
        else if (hash !== ref.hash_full) {
          const d = nativeDiff(file, f.data);
          assert.ok(d.max <= 1 && d.fraction < 0.005, `${basename(file)}: ${JSON.stringify(d)}`);
          console.log(`# ${basename(file)} (lossy JPEG XL): ${d.count} samples (${(d.fraction * 100).toFixed(2)} %) differ by 1 from native x86-64`);
        }
      }
      raw.close();
    }
    assert.equal(_internals().jpegxl.loads, 1, 'loaded exactly once');
    assert.equal((await getCapabilities()).jpegxl.binary, `${small}-jxl`);
  });

  test('close() while the file moves to the JPEG XL engine', { skip: noJxl }, async () => {
    const raw = await FlashRaw.openFile(QUICK[0]);
    const p = raw.decode();
    raw.close();
    await assert.rejects(p, { kind: 'Closed' });
    // Zero-copy previews keep pointing at the original copy after the move.
    const again = await FlashRaw.openFile(JXL_FILES.at(-1));
    const preview = await again.preview({ zeroCopy: true });
    const before = preview && preview.data.slice();
    await again.decode();
    if (preview) assert.deepEqual(preview.data, before);
    again.close();
    assert.equal(_internals().jpegxl.loads, 1);
  });
});

describe('JPEG XL modes and binaries (child processes)', () => {
  const order = [...PLAIN, ...JXL_FILES, LAST];

  for (const [label, env, small] of [
    ['single-threaded', { FLASHRAW_WASM_THREADS: '0' }, 'simd'],
    ['threaded', { FLASHRAW_WASM_THREADS: '4' }, 'threads'],
  ]) {
    test(`auto, ${label}: loads ${small}-jxl once, only for JPEG XL files`, { skip: noJxl }, () => {
      const r = run(env, order);
      assert.equal(r.info.binary, small);
      assert.equal(r.info.jpegxl, false);
      const f = byFile(r);
      for (const file of PLAIN) {
        assert.equal(f[file].loads, 0, `${basename(file)} must not load the JPEG XL variant`);
        assert.deepEqual([f[file].backend.binary, f[file].decodedIn], [small, small]);
      }
      for (const file of JXL_FILES) {
        const x = f[file];
        assert.equal(x.error, undefined, x.error?.message);
        assert.equal(x.openedIn, small);
        assert.equal(x.preview >= 0, true);
        assert.deepEqual(x.backend.binary, `${small}-jxl`);
        assert.equal(x.backend.jpegxl, true);
        assert.equal(x.loads, 1);
        assert.equal(x.same, true);
        assert.deepEqual(x.region, [64, 32]);
        if (reference[file]) assert.equal(x.hash, reference[file], basename(file));
      }
      assert.equal(f[JXL_FILES[0]].loadsBeforeDecode, 0, 'opening (metadata, previews) does not load it');
      assert.deepEqual([f[LAST].backend.binary, f[LAST].hash], [small, expected['bmpcc-tiles.dng'].hash_full]);
      assert.equal(r.internals.loads, 1);
      assert.equal(r.internals.binary, `${small}-jxl`);
      if (small === 'threads') assert.equal(r.internals.threads, 4, 'same thread count as the primary pool');
      assert.deepEqual([r.jpegxl.loaded, r.jpegxl.binary, r.jpegxl.compiled], [true, `${small}-jxl`, false]);
    });

    test(`always, ${label}: the JPEG XL variant is the only binary`, () => {
      const r = run({ ...env, FLASHRAW_WASM_JPEGXL: 'always' }, [PLAIN[0], ...QUICK]);
      assert.equal(r.info.binary, `${small}-jxl`);
      assert.equal(r.info.jpegxl, true);
      assert.equal(r.info.jpegxlMode, 'always');
      for (const x of r.files) {
        assert.equal(x.error, undefined, x.error?.message);
        assert.deepEqual([x.openedIn, x.backend.binary, x.backend.jpegxl], [`${small}-jxl`, `${small}-jxl`, true]);
        const e = expected[basename(x.file)];
        if (e) assert.equal(x.hash, e.hash_full);
        if (reference[x.file]) assert.equal(x.hash, reference[x.file]);
      }
      assert.equal(r.internals.loads, 0, 'nothing loaded on demand');
      assert.deepEqual([r.jpegxl.compiled, r.jpegxl.loaded], [true, true]);
    });
  }

  test('never: JPEG XL DNGs fail with UnsupportedCompressionError naming the option', { skip: noJxl }, () => {
    const r = run({ FLASHRAW_WASM_JPEGXL: 'never' }, [PLAIN[0], ...JXL_FILES]);
    assert.equal(r.info.jpegxlMode, 'never');
    const f = byFile(r);
    assert.equal(f[PLAIN[0]].hash, expected['canon-5d3-mlv.dng'].hash_full);
    for (const file of JXL_FILES) {
      assert.equal(f[file].format, 'DNG', 'metadata still readable');
      assert.equal(f[file].error.name, 'UnsupportedCompressionError');
      assert.equal(f[file].error.kind, 'UnsupportedCompression');
      assert.match(f[file].error.message, /JPEG XL.*jpegxl: "never".*jpegxl: "auto"/);
    }
    assert.equal(r.internals.loads, 0);
    assert.deepEqual([r.jpegxl.available, r.jpegxl.loaded], [false, false]);
  });

  test('scalar fallback build: JPEG XL needs SIMD, reported cleanly', { skip: noJxl }, () => {
    const r = run({ FLASHRAW_WASM_SIMD: '0', FLASHRAW_WASM_THREADS: '0' }, [JXL_FILES[0]]);
    assert.equal(r.info.binary, 'fallback');
    assert.equal(r.files[0].error.kind, 'UnsupportedCompression');
    assert.match(r.files[0].error.message, /require WebAssembly SIMD/);
    assert.match(r.jpegxl.reason, /require WebAssembly SIMD/);
    assert.equal(r.internals.loads, 0);
  });
});

describe('JPEG XL in the worker pool', () => {
  test('pool workers load the variant on demand', { skip: noJxl }, async () => {
    const pool = createWorkerPool({ size: 1 });
    try {
      const plain = await pool.decode(PLAIN[0]);
      assert.deepEqual([plain.backend.binary, plain.backend.jpegxl], ['simd', false]);
      assert.equal((await pool.getCapabilities()).jpegxl.loaded, false);
      const file = JXL_FILES[0];
      const f = await pool.decode(file);
      assert.deepEqual([f.backend.binary, f.backend.jpegxl], ['simd-jxl', true]);
      if (reference[file]) assert.equal(fnv1a64U16(f.data), reference[file]);
      const caps = await pool.getCapabilities();
      assert.deepEqual([caps.jpegxl.loaded, caps.jpegxl.binary], [true, 'simd-jxl']);
    } finally {
      pool.terminate();
    }
  });

  test('pool workers honour init: { jpegxl }', async () => {
    const never = createWorkerPool({ size: 1, init: { jpegxl: 'never' } });
    const always = createWorkerPool({ size: 1, init: { jpegxl: 'always' } });
    try {
      assert.equal((await never.getCapabilities()).jpegxl.mode, 'never');
      if (JXL_FILES.length) await assert.rejects(never.decode(JXL_FILES[0]), { kind: 'UnsupportedCompression', message: /jpegxl/ });
      const f = await always.decode(PLAIN[0]);
      assert.deepEqual([f.backend.binary, fnv1a64U16(f.data)], ['simd-jxl', expected['canon-5d3-mlv.dng'].hash_full]);
    } finally {
      never.terminate();
      always.terminate();
    }
  });
});
