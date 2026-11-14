// reinit(): switching builds at run time. Open handles keep their build, new handles use the new
// one, binaries are instantiated once and reused, and a refused switch keeps the old selection.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { FlashRaw, NotAvailableError, getCapabilities, init, reinit, _internals } from 'flashraw';
import { fixtures, fnv1a64U16 } from './helpers.mjs';

const file = fixtures[0];
const hashOf = async (raw, opts = { output: 'raw-u16' }) => fnv1a64U16((await raw.decode(opts)).data);

test('reinit switches builds without disturbing open handles', async () => {
  const first = await init();
  const a = await FlashRaw.openFile(file);
  assert.equal(a.binary, first.binary);
  const expected = await hashOf(a);

  // Single-threaded SIMD (a new engine next to the first one).
  const single = await reinit({ threads: false });
  assert.equal(single.binary, 'simd');
  assert.equal((await getCapabilities()).wasm.binary, 'simd');
  const b = await FlashRaw.openFile(file);
  assert.equal(b.binary, 'simd');
  assert.equal(a.binary, first.binary, 'the open handle keeps its build');
  assert.equal(await hashOf(b), expected);
  assert.equal(await hashOf(a), expected);

  // WebGPU build: no WebGPU in Node, so GPU requests fall back to the CPU with a reason.
  const gpu = await reinit({ gpu: true });
  assert.equal(gpu.binary, 'gpu');
  const g = await FlashRaw.openFile(file);
  const f = await g.decode({ output: 'raw-u16', backend: 'gpu' });
  assert.equal(f.backend.binary, 'gpu');
  assert.match(f.backend.fallbackReason, /WebGPU/);
  assert.equal(fnv1a64U16(f.data), expected);
  // A file opened in another build reports why it cannot use the GPU.
  const fb = await b.decode({ output: 'raw-u16', backend: 'gpu' });
  assert.match(fb.backend.fallbackReason, /simd build, which has no WebGPU backend/);

  // Back to the first build: reused, not instantiated again.
  const before = _internals().engines.length;
  const again = await reinit();
  assert.equal(again.binary, first.binary);
  assert.equal(_internals().engines.length, before);
  assert.ok(again.initMs < first.initMs, `reused engine: ${again.initMs} ms vs ${first.initMs} ms`);
  const c = await FlashRaw.openFile(file);
  assert.equal(c.binary, first.binary);
  assert.equal(await hashOf(c), expected);

  // The scalar build shares its glue with the SIMD one already loaded: refused, nothing changes.
  await assert.rejects(reinit({ simd: false, threads: false }), NotAvailableError);
  assert.equal((await init()).binary, first.binary);
  const d = await FlashRaw.openFile(file);
  assert.equal(d.binary, first.binary);
  assert.equal(await hashOf(d), expected);

  // Concurrent calls are serialised; the last one wins.
  const [x, y] = await Promise.all([reinit({ threads: false }), reinit({ gpu: true })]);
  assert.equal(x.binary, 'simd');
  assert.equal(y.binary, 'gpu');
  assert.equal((await init()).binary, 'gpu');

  for (const r of [a, b, g, c, d]) r.close();
});
