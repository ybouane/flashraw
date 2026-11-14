// Output colour spaces and ICC profiles: `colorSpace`, `outputProfile`, `frame.iccProfile`.
import { test, describe } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { FlashRaw, decodeRaw, InvalidArgumentError } from 'flashraw';
import { fixture } from './helpers.mjs';

const dng = new Uint8Array(readFileSync(fixture('canon-5d3-mlv.dng')));
const half = { demosaic: 'half-size' };
const maxDiff = (a, b) => a.reduce((m, v, i) => Math.max(m, Math.abs(v - b[i])), 0);

describe('colour spaces', () => {
  test('sRGB is the default and tags the frame', async () => {
    const a = await decodeRaw(dng, { output: 'rgba8', ...half });
    const b = await decodeRaw(dng, { output: 'rgba8', colorSpace: 'srgb', ...half });
    assert.equal(a.colorSpace, 'srgb');
    assert.deepEqual(a.data, b.data);
    assert.ok(a.iccProfile instanceof Uint8Array);
    assert.equal(new TextDecoder().decode(a.iccProfile.subarray(36, 40)), 'acsp');
    assert.equal((await decodeRaw(dng)).iccProfile, null);
  });

  for (const space of ['display-p3', 'adobe-rgb', 'prophoto', 'rec2020']) {
    test(`${space} changes the pixels and the profile`, async () => {
      const srgb = await decodeRaw(dng, { output: 'rgb16', ...half });
      const f = await decodeRaw(dng, { output: 'rgb16', colorSpace: space, ...half });
      assert.equal(f.colorSpace, space);
      assert.notDeepEqual(f.data, srgb.data);
      const lin = await decodeRaw(dng, { output: 'linear-rgb', colorSpace: space, ...half });
      assert.notDeepEqual(lin.iccProfile, f.iccProfile, 'linear variant differs');
    });
  }

  test('unknown spaces and malformed profiles are InvalidArgumentError', async () => {
    const raw = await FlashRaw.open(dng);
    try {
      await assert.rejects(raw.decode({ output: 'rgba8', colorSpace: 'nope' }), InvalidArgumentError);
      await assert.rejects(raw.decode({ output: 'rgba8', outputProfile: new Uint8Array(300) }), InvalidArgumentError);
      await assert.rejects(raw.decode({ output: 'rgba8', outputProfile: 'abc' }), /outputProfile/);
    } finally {
      raw.close();
    }
  });

  test('a built-in profile as outputProfile matches the built-in space', async () => {
    const p3 = await decodeRaw(dng, { output: 'rgba8', colorSpace: 'display-p3', ...half });
    const via = await decodeRaw(dng, { output: 'rgba8', outputProfile: p3.iccProfile, ...half });
    assert.equal(via.colorSpace, 'custom');
    assert.ok(maxDiff(p3.data, via.data) <= 1);
    assert.deepEqual(via.iccProfile, p3.iccProfile);
  });
});

describe('camera profiles', () => {
  const lossy = new Uint8Array(readFileSync(fixture('canon-5d3-lossy.dng')));

  test('the embedded profile changes the colours of a DNG that has one', async () => {
    const base = await decodeRaw(lossy, { output: 'rgba8', ...half });
    const prof = await decodeRaw(lossy, { output: 'rgba8', cameraProfile: 'embedded', ...half });
    assert.equal(prof.data.length, base.data.length);
    assert.notDeepEqual(prof.data, base.data);
    const lin = await decodeRaw(lossy, { output: 'linear-rgb', cameraProfile: 'embedded', ...half });
    assert.ok(lin.data.every(Number.isFinite));
  });

  test('a file without an embedded profile, and bad profiles, are InvalidArgumentError', async () => {
    await assert.rejects(decodeRaw(dng, { output: 'rgba8', cameraProfile: 'embedded' }), InvalidArgumentError);
    await assert.rejects(decodeRaw(dng, { output: 'rgba8', cameraProfile: 'nope' }), InvalidArgumentError);
    await assert.rejects(decodeRaw(dng, { output: 'rgba8', cameraProfile: new Uint8Array(64) }));
  });
});
