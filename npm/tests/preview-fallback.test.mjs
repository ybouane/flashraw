// Undecodable sensor data (Nikon High Efficiency NEF): UnsupportedCompressionError by
// default; with `previewFallback: true`, RGB outputs return the embedded full-size preview,
// marked `source: 'preview'` with a `preview-fallback` warning.
import { test, describe } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { FlashRaw, decodeRaw, UnsupportedCompressionError } from 'flashraw';
import { fixture } from './helpers.mjs';

// Synthetic NEF (crates/flashraw/tests/preview_fallback.rs): JPEG XS payload, 64x48 RGB
// preview in IFD0 (red ramp, green 100, blue 200).
const nef = new Uint8Array(readFileSync(fixture('nikon-he-synthetic.nef')));

describe('preview fallback', () => {
  test('throws UnsupportedCompressionError by default', async () => {
    for (const output of ['raw-u16', 'rgba8', 'linear-rgb']) {
      const e = await decodeRaw(nef, { output }).catch((x) => x);
      assert.ok(e instanceof UnsupportedCompressionError, `${output}: ${e}`);
    }
  });

  test('rgba8 with previewFallback: the marked preview', async () => {
    const f = await decodeRaw(nef, { output: 'rgba8', previewFallback: true });
    assert.equal(f.source, 'preview');
    assert.equal(f.incomplete, true);
    assert.equal(f.warnings.length, 1);
    assert.equal(f.warnings[0].kind, 'preview-fallback');
    assert.match(f.warnings[0].message, /embedded full-size preview/);
    assert.deepEqual([f.width, f.height, f.channels], [64, 48, 4]);
    assert.deepEqual([...f.data.subarray(0, 4)], [0, 100, 200, 255]);
    assert.equal(JSON.parse(JSON.stringify(f)).source, 'preview');
  });

  test('raw outputs and regions never fall back', async () => {
    const raw = await FlashRaw.open(nef);
    try {
      const e = await raw.decode({ output: 'raw-u16', previewFallback: true }).catch((x) => x);
      assert.ok(e instanceof UnsupportedCompressionError);
      const r = await raw
        .decode({ output: 'rgba8', previewFallback: true, region: { x: 0, y: 0, width: 8, height: 8 } })
        .catch((x) => x);
      assert.ok(r instanceof UnsupportedCompressionError);
      await assert.rejects(raw.decode({ output: 'rgba8', previewFallback: 'yes' }), /previewFallback/);
    } finally {
      raw.close();
    }
  });

  test('decodable files report the sensor source', async () => {
    const f = await decodeRaw(new Uint8Array(readFileSync(fixture('canon-a410-chdk.dng'))), {
      output: 'rgba8',
      previewFallback: true,
      demosaic: 'half-size',
    });
    assert.equal(f.source, 'sensor');
    assert.deepEqual(f.warnings, []);
  });
});
