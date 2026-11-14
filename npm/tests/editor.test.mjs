// Editor-facing API: DNG rendering hints + opcode parameters in metadata, `normalized` as half
// floats (`sampleType: 'f16'`) and raw outputs cropped to the active area (`crop: 'active'`).
import { test, describe } from 'node:test';
import assert from 'node:assert/strict';
import { FlashRaw, readMetadata, InvalidArgumentError } from 'flashraw';
import { createWorkerPool } from 'flashraw/worker';
import { fixture, corpusFile } from './helpers.mjs';

/** Reference f32 → half bits (round to nearest even, saturating). */
function half(v) {
  const f = new Float32Array(1);
  const u = new Uint32Array(f.buffer);
  f[0] = v;
  const x = u[0];
  const sign = (x >>> 16) & 0x8000;
  const a = x & 0x7fffffff;
  if (a >= 0x477ff000) return sign | (a > 0x7f800000 ? 0x7e00 : 0x7bff);
  if (a < 0x38800000) {
    f[0] = Math.abs(v) + 0.5;
    return sign | ((u[0] - 0x3f000000) & 0xffff);
  }
  return sign | (((a + 0xc8000fff + ((a >>> 13) & 1)) >>> 13) & 0xffff);
}

describe('DNG metadata for editors', () => {
  test('BaselineExposure, profile tags and opcodes', async () => {
    const m = await readMetadata(fixture('canon-5d3-lossy.dng'));
    assert.equal(m.dng.baselineExposure, 0.25);
    assert.equal(m.dng.linearResponseLimit, 1);
    assert.equal(m.dng.profile.name, 'Adobe Standard');
    assert.equal(m.dng.profile.hueSatMapCount, 2);
    assert.deepEqual(m.dng.profile.hueSatMapDims, [90, 30, 1]);
    assert.deepEqual(m.dng.profile.lookTableDims, [36, 8, 16]);
    assert.equal(m.dng.profile.toneCurve, false);
    assert.equal(m.dng.asShotNeutral.length, 3);
    assert.ok(m.opcodes.length > 0);
    for (const o of m.opcodes) {
      assert.equal(o.list, 2);
      assert.equal(o.name, 'MapPolynomial');
      assert.equal(o.applied, true);
      assert.equal(o.params, null);
    }
    // Structured-clone safe (worker pool).
    assert.deepEqual(structuredClone(m.dng), m.dng);
  });

  const pixel = corpusFile('Google/Pixel 8 Pro/PXL_20240415_103400204.RAW-02.ORIGINAL.dng');
  test('Pixel 8 Pro: AsShotWhiteXY + WarpRectilinear parameters', { skip: !pixel && 'corpus not available' }, async () => {
    const m = await readMetadata(pixel);
    assert.equal(m.dng.asShotNeutral, null);
    assert.equal(m.dng.asShotWhiteXY.length, 2);
    assert.ok(Math.abs(m.dng.asShotWhiteXY[0] - 0.322) < 0.01);
    const warp = m.opcodes.find((o) => o.name === 'WarpRectilinear');
    assert.ok(warp);
    assert.equal(warp.list, 3);
    assert.equal(warp.applied, false);
    assert.equal(warp.params.kind, 'WarpRectilinear');
    assert.equal(warp.params.planes[0].kr.length, 4);
    assert.equal(warp.params.planes[0].kt.length, 2);
    assert.ok(Math.abs(warp.params.center[0] - 0.5) < 0.01);
    assert.ok(m.opcodes.filter((o) => o.name === 'GainMap').every((o) => o.list === 2 && o.applied));
  });

  const cr2 = corpusFile('Canon/EOS 5D Mark IV/B13A0729.CR2');
  test('non-DNG files: dng null, opcodes []', { skip: !cr2 && 'corpus not available' }, async () => {
    const m = await readMetadata(cr2);
    assert.equal(m.dng, null);
    assert.deepEqual(m.opcodes, []);
  });
});

describe('16-bit and half-float RGB, exposure and black/white points', () => {
  test('linear-rgb-f16 / sampleType f16, linear-rgb16, rgb16 and levels', async () => {
    const raw = await FlashRaw.open(fixture('canon-5d3-mlv.dng'));
    const opts = { demosaic: 'half-size', exposure: 0.5, blackPoint: 0.01, whitePoint: 0.9 };
    const lin = await raw.decode({ output: 'linear-rgb', ...opts });
    for (const o of [{ output: 'linear-rgb-f16' }, { output: 'linear-rgb', sampleType: 'f16' }]) {
      const h = await raw.decode({ ...o, ...opts });
      assert.equal(h.dataType, 'f16');
      assert.equal(h.output, o.output);
      assert.ok(h.data instanceof Uint16Array);
      assert.equal(h.data.length, lin.data.length);
      for (let i = 0; i < lin.data.length; i += 89) assert.equal(h.data[i], half(lin.data[i]), `sample ${i}`);
    }
    const l16 = await raw.decode({ output: 'linear-rgb16', ...opts });
    assert.equal(l16.dataType, 'u16');
    for (let i = 0; i < lin.data.length; i += 89) {
      const want = Math.floor(Math.min(Math.max(lin.data[i], 0), 1) * 65535 + 0.5);
      assert.ok(Math.abs(l16.data[i] - want) <= 1, `sample ${i}`);
    }
    // Exposure +1 EV doubles linear values.
    const base = await raw.decode({ output: 'linear-rgb', demosaic: 'half-size' });
    const bright = await raw.decode({ output: 'linear-rgb', demosaic: 'half-size', exposure: 1 });
    for (let i = 0; i < base.data.length; i += 89) assert.equal(bright.data[i], base.data[i] * 2);
    // rgb16: display sRGB, 3 channels, oriented like rgba8.
    const rgb16 = await raw.decode({ output: 'rgb16' });
    const rgba = await raw.decode({ output: 'rgba8' });
    assert.equal(rgb16.channels, 3);
    assert.equal(rgb16.orientation, 1);
    assert.deepEqual([rgb16.width, rgb16.height], [rgba.width, rgba.height]);
    for (let p = 0; p < rgba.width * rgba.height; p += 101) {
      for (let c = 0; c < 3; c++) assert.ok(Math.abs(rgb16.data[p * 3 + c] / 257 - rgba.data[p * 4 + c]) <= 1.5);
    }
    await assert.rejects(raw.decode({ output: 'rgb16', whitePoint: 0 }), InvalidArgumentError);
    await assert.rejects(raw.decode({ output: 'rgb16', exposure: 'x' }), InvalidArgumentError);
    await assert.rejects(raw.decode({ output: 'rgba8', sampleType: 'f16' }), InvalidArgumentError);
    raw.close();
  });
});

describe('normalized f16 and active-area crop', () => {
  test('f16 equals f32 converted (fixture, full frame, zero-copy and copy)', async () => {
    const raw = await FlashRaw.open(fixture('canon-5d3-mlv.dng'));
    const f32 = await raw.decode({ output: 'normalized' });
    const f16 = await raw.decode({ output: 'normalized', sampleType: 'f16' });
    assert.equal(f16.dataType, 'f16');
    assert.ok(f16.data instanceof Uint16Array);
    assert.equal(f16.data.length, f32.data.length);
    for (let i = 0; i < f32.data.length; i += 97) assert.equal(f16.data[i], half(f32.data[i]), `sample ${i}`);
    const zc = await raw.decode({ output: 'normalized', sampleType: 'f16', zeroCopy: true });
    assert.equal(zc.zeroCopy, true);
    assert.deepEqual(zc.data.slice(0, 4096), f16.data.slice(0, 4096));
    zc.free();
    await assert.rejects(raw.decode({ output: 'raw-u16', sampleType: 'f16' }), InvalidArgumentError);
    await assert.rejects(raw.decode({ output: 'normalized', sampleType: 'f64' }), InvalidArgumentError);
    raw.close();
  });

  const cr2 = corpusFile('Canon/EOS 5D Mark IV/B13A0729.CR2');
  test("crop: 'active' (masked-area black still measured from the masked areas)", { skip: !cr2 && 'corpus not available' }, async () => {
    const raw = await FlashRaw.open(cr2);
    const a = raw.frameInfo.activeArea;
    assert.ok(a.x > 0 && a.width < raw.frameInfo.width);
    const full = await raw.decode({ output: 'normalized' });
    const act = await raw.decode({ output: 'normalized', crop: 'active', sampleType: 'f16' });
    assert.deepEqual(act.origin, { x: a.x, y: a.y });
    assert.equal(act.width, a.width);
    assert.equal(act.height, a.height);
    assert.deepEqual(act.blackLevel.values, full.blackLevel.values);
    const W = raw.frameInfo.width;
    for (const [x, y] of [[0, 0], [1, 0], [0, 1], [1234, 2345], [a.width - 1, a.height - 1]]) {
      assert.equal(act.data[y * a.width + x], half(full.data[(y + a.y) * W + x + a.x]), `(${x}, ${y})`);
    }
    // CFA phase follows the origin.
    const phase = (p, ox, oy) => p.pattern[((oy % p.height) * p.width) + (ox % p.width)];
    assert.equal(act.cfa.pattern[0], phase(full.cfa, a.x, a.y));
    const u16 = await raw.decode({ output: 'raw-u16', crop: 'active' });
    assert.equal(u16.data.length, a.width * a.height);
    raw.close();
  });

  test('worker pool passes sampleType and crop through', async () => {
    const pool = createWorkerPool({ size: 1 });
    try {
      const f = await pool.decode(fixture('canon-5d3-mlv.dng'), { output: 'normalized', sampleType: 'f16', crop: 'active' });
      assert.equal(f.dataType, 'f16');
      assert.ok(f.data instanceof Uint16Array);
      assert.equal(f.data.length, f.width * f.height * f.channels);
      const m = await pool.readMetadata(fixture('canon-5d3-lossy.dng'));
      assert.equal(m.dng.baselineExposure, 0.25);
    } finally {
      pool.terminate();
    }
  });
});
