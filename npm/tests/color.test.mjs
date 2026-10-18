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
