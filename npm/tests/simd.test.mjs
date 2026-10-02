// The scalar fallback binary must produce exactly the same output as the SIMD binary.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { fixtures } from './helpers.mjs';

const script = fileURLToPath(new URL('./simd-child.mjs', import.meta.url));

function run(env) {
  const out = execFileSync(process.execPath, [script, ...fixtures], {
    env: { ...process.env, ...env },
    encoding: 'utf8',
  });
  return JSON.parse(out);
}

test('SIMD and fallback binaries agree (raw-u16 and rgba8)', () => {
  const simd = run({ FLASHRAW_WASM_SIMD: '' });
  const scalar = run({ FLASHRAW_WASM_SIMD: '0' });
  assert.equal(simd.variant, 'simd');
  assert.equal(simd.level, 'WASM SIMD128');
  assert.equal(scalar.variant, 'fallback');
  assert.equal(scalar.level, 'scalar');
  assert.deepEqual(scalar.hashes, simd.hashes);
});
