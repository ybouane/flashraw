import { test, describe } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { basename } from 'node:path';
import { createWorkerPool } from 'flashraw/worker';
import { FlashRawError } from 'flashraw';
import { fixtures, fixture, fixturesDir, fnv1a64U16, realworldFile } from './helpers.mjs';

const expected = JSON.parse(readFileSync(`${fixturesDir}/expected.json`, 'utf8'));

describe('worker pool (worker_threads)', () => {
  test('decodes several files concurrently with identical results', async () => {
    const pool = createWorkerPool({ size: 3 });
    try {
      assert.equal(pool.size, 3);
      const jobs = [...fixtures, ...fixtures].map((f) => pool.decode(f));
      const frames = await Promise.all(jobs);
      assert.equal(pool.load.length, 3, 'lazily spawned up to size');
      frames.forEach((fr, i) => {
        const ref = expected[basename(fixtures[i % fixtures.length])];
        assert.ok(fr.data instanceof Uint16Array);
        assert.equal(fr.width, ref.width);
        assert.equal(fnv1a64U16(fr.data), ref.hash_full);
        assert.equal(fr.backend.kind, 'cpu');
      });
    } finally {
      pool.terminate();
    }
  });

  test('bytes (cloned or transferred), metadata, previews, rgba8', async () => {
    const pool = createWorkerPool({ size: 1 });
    try {
      const bytes = readFileSync(fixture('canon-5d3-mlv.dng'));
      const m = await pool.readMetadata(bytes);
      assert.equal(m.format, 'DNG');
      const p = await pool.extractPreview(bytes);
      assert.equal(p.mimeType, 'image/jpeg');
      assert.ok(p.data instanceof Uint8Array);
      const ab = new Uint8Array(bytes).buffer;
      const f = await pool.decode(ab, { transfer: true, output: 'rgba8', demosaic: 'half-size' });
      assert.equal(ab.byteLength, 0, 'input ArrayBuffer was transferred');
      assert.ok(f.data instanceof Uint8ClampedArray);
      assert.equal(f.data.length, f.width * f.height * 4);
      assert.equal(await pool.detectFormat(bytes), 'DNG');
      const caps = await pool.getCapabilities();
      assert.equal(caps.platform, 'node');
    } finally {
      pool.terminate();
    }
  });

  test('remote handles keep a file open in one worker', async () => {
    const pool = createWorkerPool({ size: 2 });
    try {
      const raw = await pool.open(fixture('bmpcc-tiles.dng'));
      assert.equal(raw.metadata.camera.canonicalMake, 'Blackmagic');
      assert.equal(raw.tiles.length, 2);
      const [a, b] = await Promise.all([raw.decodeTile(0), raw.decodeTile(1)]);
      assert.equal(a.width + b.width, raw.frameInfo.width);
      const reg = await raw.decodeRegion({ x: 10, y: 10, width: 32, height: 16 });
      assert.equal(reg.data.length, 32 * 16);
      assert.equal(await raw.preview(), null);
      assert.equal(await raw.exifBytes(), null);
      assert.equal(await raw.xmpBytes(), null);
      const mlv = await pool.open(fixture('canon-5d3-mlv.dng'));
      const exif = await mlv.exifBytes();
      assert.ok(exif instanceof Uint8Array && exif.length > 8);
      await mlv.close();
      await raw.close();
      await assert.rejects(raw.decode(), { kind: 'Closed' });
    } finally {
      pool.terminate();
    }
  });

  test('errors cross the worker boundary as structured errors', async () => {
    const pool = createWorkerPool({ size: 1, init: { simd: false } });
    try {
      const e = await pool.decode(new Uint8Array(1000)).catch((x) => x);
      assert.ok(e instanceof FlashRawError);
      assert.equal(e.kind, 'UnsupportedFormat');
      const caps = await pool.getCapabilities();
      assert.equal(caps.wasm.variant, 'fallback', 'init options reach the worker');
      await assert.rejects(pool.decode(fixture('canon-5d3-mlv.dng'), { backend: 'gpu', fallback: 'error' }), {
        kind: 'GpuUnavailable',
      });
    } finally {
      pool.terminate();
    }
    await assert.rejects(pool.decode(fixture('canon-5d3-mlv.dng')), { kind: 'Closed' });
  });

  test('cancellation: queued requests are dropped, running decodes stop and keep their worker', async () => {
    const pool = createWorkerPool({ size: 1 });
    try {
      const file = fixture('canon-5d3-mlv.dng');
      const ref = expected['canon-5d3-mlv.dng'].hash_full;
      // Already aborted: rejected without touching a worker.
      const pre = new AbortController();
      pre.abort();
      await assert.rejects(pool.decode(file, { signal: pre.signal }), { name: 'AbortError' });
      assert.equal(pool.load.length, 0);
      await assert.rejects(pool.decode(file, { signal: 'nope' }), { kind: 'InvalidArgument' });

      // Queued behind another request: dropped; the running one is unaffected.
      const a = pool.decode(file);
      const c = new AbortController();
      const b = pool.decode(file, { signal: c.signal });
      assert.deepEqual([pool.load, pool.queued], [[1], 1]);
      c.abort(new Error('not needed'));
      await assert.rejects(b, { message: 'not needed' });
      assert.equal(fnv1a64U16((await a).data), ref);

      // Running decode: it is asked to stop, the request rejects with the abort reason, and the
      // worker, with the file open in it, stays usable (the queued request behind it runs).
      const raw = await pool.open(file);
      const running = new AbortController();
      const p = raw.decode({ output: 'rgba8', signal: running.signal });
      const queued = raw.decode();
      running.abort();
      await assert.rejects(p, { name: 'AbortError' });
      assert.equal(fnv1a64U16((await queued).data), ref);
      assert.equal(raw.closed, false);
      assert.equal(fnv1a64U16((await raw.decode()).data), ref);
      await raw.close();

      // Running something that cannot poll the flag: its worker is terminated at once, its
      // handles die with it, and a fresh worker takes over.
      const raw2 = await pool.open(file);
      const t0 = performance.now();
      const other = new AbortController();
      const m = raw2.exifBytes({ signal: other.signal });
      const queued2 = raw2.decode(); // waits on the same worker: its handle dies with it
      other.abort();
      await assert.rejects(m, { name: 'AbortError' });
      await assert.rejects(queued2, { kind: 'Closed' });
      assert.ok(performance.now() - t0 < 1000);
      assert.equal(raw2.closed, true);
      await assert.rejects(raw2.decode(), { kind: 'Closed' });
      assert.equal(fnv1a64U16((await pool.decode(file)).data), ref, 'a fresh worker took over');
      // Timeouts are signals too.
      const late = await pool.decode(file, { signal: AbortSignal.timeout(60_000) });
      assert.equal(fnv1a64U16(late.data), ref);
    } finally {
      pool.terminate();
    }
  });

  test('a decode in progress stops within milliseconds and the worker survives', async (ctx) => {
    const big = realworldFile('Fujifilm/GFX100ShSLI_ADOBE.RAF') ?? realworldFile('Phase_One/Y-JG-P1-XT-150MP-ACHROMATIC-13826.IIQ');
    if (!big) return ctx.skip('no large real-world file ($FLASHRAW_REALWORLD)');
    const pool = createWorkerPool({ size: 1 });
    try {
      const raw = await pool.open(big);
      const full0 = performance.now();
      await raw.decode({ output: 'rgba8' });
      const full = performance.now() - full0;
      const c = new AbortController();
      const p = raw.decode({ output: 'rgba8', signal: c.signal });
      await new Promise((r) => setTimeout(r, Math.min(80, full / 4)));
      const t0 = performance.now();
      c.abort();
      await assert.rejects(p, { name: 'AbortError' });
      const stopped = performance.now() - t0;
      assert.ok(stopped < 400 && stopped < full / 2, `stopped in ${stopped.toFixed(0)} ms (a full decode takes ${full.toFixed(0)} ms)`);
      assert.equal(raw.closed, false, 'the worker was not terminated');
      assert.equal((await raw.decode({ output: 'raw-u16' })).width > 0, true);
    } finally {
      pool.terminate();
    }
  });

  test('a handle from a terminated worker never reaches its replacement', async () => {
    const pool = createWorkerPool({ size: 1 });
    try {
      const a = await pool.open(fixture('canon-5d3-mlv.dng'));
      const c = new AbortController();
      const p = a.exifBytes({ signal: c.signal }); // not a decode: stopping it terminates the worker
      c.abort();
      await assert.rejects(p, { name: 'AbortError' });
      // The replacement worker numbers its handles from 1 again.
      const b = await pool.open(fixture('bmpcc-tiles.dng'));
      await assert.rejects(a.decode(), { kind: 'Closed' });
      await a.close();
      assert.equal(b.metadata.camera.canonicalMake, 'Blackmagic');
      assert.equal((await b.decode()).width, b.frameInfo.width);
    } finally {
      pool.terminate();
    }
  });

  test('requests go to whichever worker is free', async () => {
    const pool = createWorkerPool({ size: 2 });
    try {
      const jobs = Array.from({ length: 6 }, () => pool.readMetadata(fixture('bmpcc-tiles.dng')));
      assert.deepEqual(pool.load, [1, 1]);
      assert.equal(pool.queued, 4);
      for (const m of await Promise.all(jobs)) assert.equal(m.format, 'DNG');
      assert.deepEqual([pool.load, pool.queued], [[0, 0], 0]);
    } finally {
      pool.terminate();
    }
  });
});
