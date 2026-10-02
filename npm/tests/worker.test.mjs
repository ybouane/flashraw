import { test, describe } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { basename } from 'node:path';
import { createWorkerPool } from 'flashraw/worker';
import { FlashRawError } from 'flashraw';
import { fixtures, fixture, fixturesDir, fnv1a64U16 } from './helpers.mjs';

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
});
