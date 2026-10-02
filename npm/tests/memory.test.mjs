import { test, describe } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { FlashRaw, decodeRaw, init, _internals } from '../dist/node.js';
import { fixture } from './helpers.mjs';

const memBytes = () => _internals().wasm.memory.buffer.byteLength;

describe('memory release', () => {
  test('open/decode/close cycles do not grow WASM memory', async () => {
    await init();
    const bytes = readFileSync(fixture('canon-5d3-mlv.dng'));
    const cycle = async () => {
      const raw = await FlashRaw.open(bytes);
      await raw.decode();
      await raw.decode({ output: 'rgba8', demosaic: 'half-size' });
      await raw.preview();
      raw.close();
    };
    // Warm up: the allocator grows memory to the high-water mark once.
    for (let i = 0; i < 3; i++) await cycle();
    const base = memBytes();
    for (let i = 0; i < 25; i++) await cycle();
    assert.equal(memBytes(), base, 'WASM memory grew across cycles: something is not freed');
    assert.equal(_internals().liveCount, 0);
  });

  test('zero-copy frames are pinned until free(), then reusable', async () => {
    const raw = await FlashRaw.openFile(fixture('canon-5d3-mlv.dng'));
    const frames = [];
    for (let i = 0; i < 3; i++) frames.push(await raw.decode({ zeroCopy: true }));
    assert.equal(_internals().liveCount, 3);
    const high = memBytes();
    for (const f of frames) f.free();
    assert.equal(_internals().liveCount, 0);
    // Freed buffers are reused: decoding the same amount again needs no new memory.
    for (let i = 0; i < 3; i++) frames[i] = await raw.decode({ zeroCopy: true });
    assert.equal(memBytes(), high);
    frames.forEach((f) => f.free());
    // A zero-copy preview pins the handle until close().
    await raw.preview({ zeroCopy: true });
    assert.equal(_internals().liveCount, 1);
    raw.close();
    assert.equal(_internals().liveCount, 0);
  });

  test('frames dropped without close are reclaimed by the FinalizationRegistry', async (t) => {
    if (typeof globalThis.gc !== 'function') return t.skip('run node with --expose-gc');
    const bytes = readFileSync(fixture('canon-5d3-mlv.dng'));
    for (let i = 0; i < 3; i++) await decodeRaw(bytes);
    const base = memBytes();
    for (let i = 0; i < 20; i++) {
      // Intentionally leaked handles (never closed).
      const raw = await FlashRaw.open(bytes);
      await raw.decode();
      if (i % 4 === 0) {
        globalThis.gc();
        await new Promise((r) => setTimeout(r, 0));
      }
    }
    globalThis.gc();
    await new Promise((r) => setTimeout(r, 10));
    // Memory is bounded (not 20x a file): leaked handles were freed by GC.
    assert.ok(memBytes() <= base + 4 * bytes.length + (16 << 20), `${memBytes()} vs ${base}`);
  });
});
