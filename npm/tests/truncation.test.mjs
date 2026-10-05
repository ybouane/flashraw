// Truncated sensor data: recovered with warnings by default, a structured
// CorruptFileError with `corruptionPolicy: 'strict'`.
import { test, describe } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { FlashRaw, decodeRaw, readMetadata, CorruptFileError, InvalidArgumentError } from 'flashraw';
import { createWorkerPool } from 'flashraw/worker';
import { fixture } from './helpers.mjs';

// 2144x1560 10-bit packed CHDK DNG; its payload ends at the end of the file.
const full = new Uint8Array(readFileSync(fixture('canon-a410-chdk.dng')));
const cut = full.slice(0, full.length - 1000); // 1000 bytes = 800 10-bit samples

describe('truncated sensor data', () => {
  test('complete file: no warnings under either policy', async () => {
    for (const corruptionPolicy of ['recover', 'strict']) {
      const f = await decodeRaw(full, { corruptionPolicy });
      assert.equal(f.incomplete, false);
      assert.deepEqual(f.warnings, []);
    }
  });

  test('recover (default): zero-filled samples and a structured warning', async () => {
    const f = await decodeRaw(cut, { applyOpcodes: false }); // opcodes would touch the zeros
    assert.equal(f.incomplete, true);
    assert.equal(f.warnings.length, 1);
    const w = f.warnings[0];
    assert.equal(w.kind, 'truncated');
    assert.equal(w.unit, 'strip');
    assert.equal(w.index, 0);
    assert.equal(w.expected - w.available, 1000);
    assert.equal(w.fileLength, cut.length);
    assert.equal(w.samplesFilled, 800);
    assert.match(w.message, /^recovered truncated strip 0: \d+ \/ \d+ bytes available/);
    assert.ok(f.data.subarray(f.data.length - 800).every((v) => v === 0));
    const ref = await decodeRaw(full);
    assert.deepEqual(f.data.subarray(0, 1000), ref.data.subarray(0, 1000));
    // Survives JSON (worker messages, logging).
    assert.deepEqual(JSON.parse(JSON.stringify(f)).warnings, f.warnings);
  });

  test('strict: CorruptFileError with the truncation details', async () => {
    const e = await decodeRaw(cut, { corruptionPolicy: 'strict' }).catch((x) => x);
    assert.ok(e instanceof CorruptFileError, String(e));
    assert.equal(e.kind, 'CorruptFile');
    assert.match(e.message, /truncated image data: strip 0 expected \d+ bytes at offset \d+, but only \d+ bytes remain before EOF/);
    assert.equal(e.corrupt.kind, 'truncated');
    assert.equal(e.corrupt.expected - e.corrupt.available, 1000);
    assert.equal(e.corrupt.fileLength, cut.length);
    assert.deepEqual(JSON.parse(JSON.stringify(e)).corrupt, e.corrupt);
  });

  test('metadata and region decodes do not need the missing data', async () => {
    const m = await readMetadata(cut);
    assert.equal(m.width > 0, true);
    const raw = await FlashRaw.open(cut);
    try {
      const top = await raw.decode({ region: { x: 0, y: 0, width: 64, height: 64 }, corruptionPolicy: 'strict' });
      assert.deepEqual(top.warnings, []);
    } finally {
      raw.close?.();
    }
  });

  test('unknown policy is an InvalidArgumentError', async () => {
    await assert.rejects(decodeRaw(full, { corruptionPolicy: 'lenient' }), InvalidArgumentError);
  });

  test('worker pool keeps warnings and error details', async () => {
    const pool = createWorkerPool({ size: 1 });
    try {
      const f = await pool.decode(cut.slice());
      assert.equal(f.incomplete, true);
      assert.equal(f.warnings[0].samplesFilled, 800);
      const e = await pool.decode(cut.slice(), { corruptionPolicy: 'strict' }).catch((x) => x);
      assert.ok(e instanceof CorruptFileError, String(e));
      assert.equal(e.corrupt.expected - e.corrupt.available, 1000);
    } finally {
      await pool.terminate?.();
    }
  });
});
