// Multithreaded WASM build (flashraw_simd_threads.wasm): output must be identical to the
// single-threaded build for every output, and raw-u16 identical to the native CLI.
import { test, describe } from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { basename } from 'node:path';
import { fileURLToPath } from 'node:url';
import { FlashRaw, init, initThreads, getCapabilities, _internals } from 'flashraw';
import { fixtures, fixture, fixturesDir, corpusFile, fnv1a64U16, nativeVerify } from './helpers.mjs';

const script = fileURLToPath(new URL('./variant-child.mjs', import.meta.url));
const expected = JSON.parse(readFileSync(`${fixturesDir}/expected.json`, 'utf8'));

// Formats whose decoders use rayon differently: CRX planes, Nikon Huffman, Sony ARW2 rows,
// uncompressed RAF, Panasonic v4 blocks, Olympus pipelined bands.
const CORPUS = [
  'Canon/EOS R5/Canon_EOS_R5_CRAW_ISO_100_crop_nodual.CR3',
  'Nikon/Z 7/8-Nikon-Z7-RAW-12bit-lossless-compressed-S.NEF',
  'Sony/ILCE-7M4/ILCE-7M4_DSC06681_APS-C-Raw-Compressed.ARW',
  'Fujifilm/X-T3/AFXT2720.RAF',
  'Panasonic/DC-GH5/_T012010.RW2',
  // Olympus: the decoder overlaps entropy decoding and prediction; it used std::thread under
  // rayon, which traps in WASM ("failed to spawn thread").
  'Olympus/E-M1MarkII/Olympus_EM1mk2_Standard_20MP.ORF',
  'OM System/OM-1/OM_Systems_OM1_RAW_Image_20mpx_P1010025.ORF',
]
  .map(corpusFile)
  .filter(Boolean);
const FILES = [...fixtures, ...CORPUS];

function run(env) {
  const out = execFileSync(process.execPath, [script, ...FILES], {
    env: { ...process.env, FLASHRAW_WASM_SIMD: '', ...env },
    encoding: 'utf8',
    maxBuffer: 1 << 24,
  });
  return JSON.parse(out);
}

describe('threaded build', () => {
  test('threaded and single-threaded binaries agree on every output; raw-u16 == native CLI', () => {
    const single = run({ FLASHRAW_WASM_THREADS: '0' });
    const threaded = run({ FLASHRAW_WASM_THREADS: '4' });
    assert.equal(single.binary, 'simd');
    assert.equal(single.threads, 0);
    assert.equal(threaded.binary, 'threads');
    assert.equal(threaded.threads, 4);
    assert.equal(threaded.level, 'WASM SIMD128');
    for (const file of FILES) {
      assert.deepEqual(threaded.hashes[file], single.hashes[file], basename(file));
      const ref = nativeVerify(file) ?? expected[basename(file)];
      if (ref?.hash_full) assert.equal(threaded.hashes[file].u16, ref.hash_full, `${basename(file)} vs native CLI`);
    }
    const sum = (r, k) => FILES.reduce((n, f) => n + (r.ms[f][k] ?? 0), 0);
    console.log(
      `# rgba8 total: single ${sum(single, 'rgba8').toFixed(0)} ms, 4 threads ${sum(threaded, 'rgba8').toFixed(0)} ms` +
        ` (${FILES.length} files)`,
    );
  });

  test('default thread count follows the hardware; FLASHRAW_WASM_THREADS overrides it', () => {
    const r = JSON.parse(
      execFileSync(process.execPath, [script], { env: { ...process.env, FLASHRAW_WASM_THREADS: '' }, encoding: 'utf8' }),
    );
    assert.equal(r.binary, 'threads');
    assert.ok(r.threads >= 1);
  });
});

describe('threaded build in this process', { skip: process.env.FLASHRAW_WASM_THREADS === '0' && 'single-threaded run' }, () => {
  test('initThreads / capabilities', async () => {
    const n = await initThreads();
    const caps = await getCapabilities();
    assert.equal(caps.threads.count, n);
    assert.equal(caps.threads.wasmThreads, n > 0);
    assert.equal(caps.wasm.binary, n > 0 ? 'threads' : caps.wasm.variant);
    // Threads are chosen once: later calls report the pool in use.
    assert.equal(await initThreads(2), n);
    assert.equal(Object.prototype.toString.call(_internals().wasm.memory.buffer), n > 0 ? '[object SharedArrayBuffer]' : '[object ArrayBuffer]');
  });

  test('concurrent decodes on one handle and on several handles', async () => {
    await init();
    const file = fixture('canon-5d3-mlv.dng');
    const want = expected['canon-5d3-mlv.dng'].hash_full;
    const raw = await FlashRaw.openFile(file);
    const others = await Promise.all([1, 2, 3].map(() => FlashRaw.openFile(file)));
    const frames = await Promise.all([
      raw.decode(),
      raw.decode({ output: 'rgba8' }),
      raw.decode(),
      ...others.map((r) => r.decode()),
    ]);
    for (const i of [0, 2, 3, 4, 5]) assert.equal(fnv1a64U16(frames[i].data), want);
    assert.equal(frames[1].data.length, frames[1].width * frames[1].height * 4);
    raw.close();
    others.forEach((r) => r.close());
  });

  test('close() while a decode is running is safe', async () => {
    const raw = await FlashRaw.openFile(fixture('bmpcc-tiles.dng'));
    const p = raw.decode({ output: 'rgba8' });
    raw.close();
    const f = await p;
    assert.ok(f.data.length > 0);
    await assert.rejects(raw.decode(), { kind: 'Closed' });
  });

  test('zero-copy frames, tiles, previews and errors work across threads', async () => {
    const raw = await FlashRaw.openFile(fixture('bmpcc-tiles.dng'));
    const z = await raw.decode({ zeroCopy: true });
    assert.equal(fnv1a64U16(z.data), expected['bmpcc-tiles.dng'].hash_full);
    z.free();
    const tile = await raw.decodeTile(0);
    assert.equal(tile.width, raw.tiles[0].rect.width);
    const p = await raw.preview({ format: 'rgba8' });
    if (p) assert.equal(p.data.length, p.width * p.height * 4);
    await assert.rejects(raw.decode({ output: 'png' }), { kind: 'InvalidArgument' });
    await assert.rejects(raw.decodeTile(9999), { kind: 'InvalidArgument' });
    raw.close();
    await assert.rejects(FlashRaw.open(new Uint8Array(4096)), { kind: 'UnsupportedFormat' });
    // The pool survives errors.
    const again = await FlashRaw.openFile(fixture('canon-a410-chdk.dng'));
    assert.equal(fnv1a64U16((await again.decode()).data), expected['canon-a410-chdk.dng'].hash_full);
    again.close();
  });
});
