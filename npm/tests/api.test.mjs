import { test, describe, before } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { basename } from 'node:path';
import { pathToFileURL } from 'node:url';
import {
  FlashRaw,
  DecodedFrame,
  decodeRaw,
  readMetadata,
  extractPreview,
  extractThumbnail,
  supportsFile,
  detectFormat,
  getCapabilities,
  init,
  simdSupported,
  FlashRawError,
  GpuUnavailableError,
  OUTPUTS,
  _internals,
} from 'flashraw';
import { fixtures, fixture, fnv1a64U16, nativeVerify, fixturesDir } from './helpers.mjs';

const expected = JSON.parse(readFileSync(`${fixturesDir}/expected.json`, 'utf8'));

/** Native CLI result if available (live), else the recorded one. */
function reference(file) {
  const live = nativeVerify(file);
  if (live) {
    return {
      format: live.format,
      make: live.canonical_make,
      model: live.canonical_model,
      width: live.raw_width,
      height: live.raw_height,
      cpp: live.cpp,
      cfa: live.cfa,
      white: live.white,
      hash_full: live.hash_full,
      previews: live.previews,
      live: true,
    };
  }
  return { ...expected[basename(file)], live: false };
}

before(async () => {
  const info = await init();
  assert.equal(info.variant, simdSupported() && process.env.FLASHRAW_WASM_SIMD !== '0' ? 'simd' : 'fallback');
});

describe('capabilities', () => {
  test('getCapabilities reports build and runtime features', async () => {
    const c = await getCapabilities();
    assert.equal(c.platform, 'node');
    assert.match(c.version, /^\d+\.\d+\.\d+/);
    assert.equal(c.wasm.loaded, true);
    assert.equal(c.simd.supported, true);
    assert.equal(c.simd.active, c.wasm.variant === 'simd');
    assert.equal(c.gpu.compiled, false);
    assert.equal(c.gpu.available, false);
    assert.match(c.gpu.reason, /gpu/i);
    // Node: the threaded build is the default (FLASHRAW_WASM_THREADS=0 selects single-threaded).
    const threaded = !['0', 'false', 'off'].includes(process.env.FLASHRAW_WASM_THREADS) && c.simd.active;
    assert.equal(c.threads.wasmThreads, threaded);
    assert.equal(c.wasm.binary, threaded ? 'threads' : c.wasm.variant);
    if (threaded) {
      assert.ok(c.threads.count >= 1);
      assert.equal(c.threads.reason, null);
    } else {
      assert.equal(c.threads.count, 0);
      assert.equal(typeof c.threads.reason, 'string');
    }
    assert.equal(c.threads.workers, true);
    assert.deepEqual(c.outputs, [...OUTPUTS]);
  });
});

for (const file of fixtures) {
  const name = basename(file);
  describe(name, () => {
    const ref = reference(file);

    test('metadata matches the native decoder', async () => {
      const raw = await FlashRaw.openFile(file);
      try {
        const m = raw.metadata;
        assert.equal(m.format, ref.format);
        assert.equal(raw.format, ref.format);
        assert.equal(m.camera.canonicalMake, ref.make);
        assert.equal(m.camera.canonicalModel, ref.model);
        assert.equal(m.rawWidth, ref.width);
        assert.equal(m.rawHeight, ref.height);
        assert.equal(m.cfa?.pattern ?? null, ref.cfa);
        assert.equal(Math.max(...m.whiteLevel), ref.white);
        assert.equal(m.previews.length, ref.previews);
        assert.ok(m.width > 0 && m.height > 0);
        assert.ok([1, 2, 3, 4, 5, 6, 7, 8].includes(m.orientation));
        assert.equal(typeof m.compression, 'string');
        assert.ok(Array.isArray(m.colorMatrices.color));
        // JSON-compatible: survives a round trip unchanged.
        assert.deepEqual(JSON.parse(JSON.stringify(m)), m);
        // Cached: same object on repeated access.
        assert.equal(raw.metadata, m);
        const fi = raw.frameInfo;
        assert.equal(fi.width, ref.width);
        assert.equal(fi.channels, ref.cpp);
        assert.ok(raw.capabilities.randomAccess.kind);
        assert.equal(raw.tiles.length, raw.capabilities.tiles);
        assert.equal(raw.byteLength, readFileSync(file).length);
      } finally {
        raw.close();
      }
    });

    test('raw-u16 decode is bit-identical to the native CLI (FNV-1a hash_full)', async () => {
      const bytes = readFileSync(file);
      const raw = await FlashRaw.open(bytes);
      const frame = await raw.decode({ backend: 'auto', output: 'raw-u16' });
      raw.close();
      assert.ok(frame instanceof DecodedFrame);
      assert.ok(frame.data instanceof Uint16Array);
      assert.equal(frame.width, ref.width);
      assert.equal(frame.height, ref.height);
      assert.equal(frame.channels, ref.cpp);
      assert.equal(frame.data.length, ref.width * ref.height * ref.cpp);
      assert.equal(frame.cfa?.pattern ?? null, ref.cfa);
      assert.equal(fnv1a64U16(frame.data), ref.hash_full, ref.live ? 'vs live CLI' : 'vs recorded CLI hash');
      assert.equal(frame.backend.kind, 'cpu');
      assert.equal(frame.backend.requested, 'auto');
      assert.match(frame.backend.decode, /^CPU /);
      assert.equal(frame.backend.threads, Math.max(1, _internals().threads));
      assert.ok(frame.timings.totalMs > 0, 'performance.now() clock is installed');
      assert.ok(Object.keys(frame.timings.stages).length > 0);
      assert.ok(frame.timings.wallMs >= frame.timings.copyMs);
      assert.equal(frame.zeroCopy, false);
      assert.equal(frame.outputBytes, frame.data.byteLength);
    });

    test('scalar backend gives the same pixels', async () => {
      const f = await decodeRaw(file, { backend: 'cpu' });
      assert.equal(fnv1a64U16(f.data), ref.hash_full);
      assert.equal(f.backend.simd, 'scalar');
    });

    test('all output formats', async () => {
      const raw = await FlashRaw.openFile(file);
      try {
        const f32 = await raw.decode({ output: 'raw-f32' });
        assert.ok(f32.data instanceof Float32Array);
        assert.equal(f32.data.length, ref.width * ref.height * ref.cpp);
        const n = await raw.decode({ output: 'normalized' });
        assert.ok(n.data instanceof Float32Array);
        let max = 0;
        for (let i = 0; i < n.data.length; i += 97) max = Math.max(max, n.data[i]);
        assert.ok(max <= 1.0001 && max > 0, `normalized max ${max}`);
        const lin = await raw.decode({ output: 'linear-rgb', demosaic: 'half-size' });
        assert.ok(lin.data instanceof Float32Array);
        assert.equal(lin.channels, 3);
        assert.equal(lin.data.length, lin.width * lin.height * 3);
        const rgba = await raw.decode({ output: 'rgba8' });
        assert.ok(rgba.data instanceof Uint8ClampedArray);
        assert.equal(rgba.channels, 4);
        assert.equal(rgba.data.length, rgba.width * rgba.height * 4);
        assert.equal(rgba.orientation, 1);
        assert.equal(rgba.cfa, null);
        assert.ok(rgba.timings.stages.postprocess > 0);
        // Opaque alpha and a non-black image.
        let sum = 0;
        for (let i = 0; i < rgba.data.length; i += 4 * 101) {
          assert.equal(rgba.data[i + 3], 255);
          sum += rgba.data[i] + rgba.data[i + 1] + rgba.data[i + 2];
        }
        assert.ok(sum > 0);
        const cropped = await raw.decode({ output: 'rgba8', crop: true });
        const full = await raw.decode({ output: 'rgba8', crop: false });
        assert.ok(full.width * full.height >= cropped.width * cropped.height);
      } finally {
        raw.close();
      }
    });

    test('region and tile decodes match the full frame', async () => {
      const raw = await FlashRaw.openFile(file);
      try {
        const full = await raw.decode();
        const r = { x: 34, y: 18, width: 100, height: 60 };
        const reg = await raw.decodeRegion(r);
        assert.equal(reg.width, r.width);
        assert.equal(reg.height, r.height);
        assert.deepEqual(reg.origin, { x: r.x, y: r.y });
        const cpp = full.channels;
        for (let y = 0; y < r.height; y += 7) {
          for (let x = 0; x < r.width * cpp; x += 5) {
            assert.equal(reg.data[y * r.width * cpp + x], full.data[(y + r.y) * full.width * cpp + r.x * cpp + x]);
          }
        }
        const tiles = raw.tiles;
        assert.ok(tiles.length >= 1);
        const t = tiles[tiles.length - 1];
        const tf = await raw.decodeTile(t.index);
        assert.equal(tf.width, t.rect.width);
        assert.equal(tf.height, t.rect.height);
        assert.deepEqual(tf.origin, { x: t.rect.x, y: t.rect.y });
        assert.equal(tf.data[0], full.data[(t.rect.y * full.width + t.rect.x) * cpp]);
        await assert.rejects(raw.decodeTile(tiles.length), { kind: 'InvalidArgument' });
      } finally {
        raw.close();
      }
    });

    test('previews', async () => {
      const raw = await FlashRaw.openFile(file);
      try {
        const p = await raw.preview();
        const t = await raw.thumbnail();
        if (ref.previews === 0) {
          assert.equal(p, null);
          assert.equal(t, null);
          assert.equal(await extractPreview(file), null);
          return;
        }
        for (const x of [p, t]) {
          assert.ok(x.data instanceof Uint8Array);
          assert.equal(x.data.length, x.byteLength);
          assert.equal(typeof x.mimeType, 'string');
          assert.ok([1, 2, 3, 4, 5, 6, 7, 8].includes(x.orientation));
          if (x.mimeType === 'image/jpeg') assert.deepEqual([...x.data.subarray(0, 2)], [0xff, 0xd8]);
        }
        assert.ok(p.width * p.height >= t.width * t.height);
        // Copies are independent of the handle.
        const keep = p.data;
        raw.close();
        assert.ok(keep.length > 0 && keep.buffer.byteLength === keep.length);
        const rgba = await extractThumbnail(file, { format: 'rgba8' });
        assert.ok(rgba.data instanceof Uint8ClampedArray);
        assert.equal(rgba.data.length, rgba.width * rgba.height * 4);
        assert.equal((await extractPreview(file)).byteLength, p.byteLength);
      } finally {
        raw.close();
      }
    });
  });
}

describe('inputs', () => {
  const file = fixture('canon-5d3-mlv.dng');
  const buf = readFileSync(file);
  const ref = expected['canon-5d3-mlv.dng'];

  test('accepts Buffer, Uint8Array, ArrayBuffer, offset views, DataView, Blob, path and file URL', async () => {
    const padded = new Uint8Array(buf.length + 13);
    padded.set(buf, 7);
    const inputs = [
      buf,
      new Uint8Array(buf),
      new Uint8Array(buf).buffer,
      padded.subarray(7, 7 + buf.length),
      new DataView(padded.buffer, 7, buf.length),
      new Blob([buf]),
      file,
      pathToFileURL(file),
    ];
    for (const input of inputs) {
      const m = await readMetadata(input);
      assert.equal(m.rawWidth, ref.width, Object.prototype.toString.call(input));
    }
    const f = await decodeRaw(new Blob([buf]));
    assert.equal(fnv1a64U16(f.data), ref.hash_full);
  });

  test('a Blob that fails to stream is read in slices', async () => {
    // WebKit rejects stream()/arrayBuffer() of large fetched Blobs (NotReadableError), even
    // part-way through, while slice() reads work.
    class Flaky extends Blob {
      stream() {
        let sent = false;
        return new ReadableStream({
          pull(c) {
            if (sent) c.error(new DOMException('The I/O read operation failed.', 'NotReadableError'));
            else {
              sent = true;
              c.enqueue(new Uint8Array(1000));
            }
          },
        });
      }
    }
    const f = await decodeRaw(new Flaky([buf]));
    assert.equal(fnv1a64U16(f.data), ref.hash_full);
  });

  test('input is copied: mutating the caller buffer after open has no effect', async () => {
    const copy = new Uint8Array(buf);
    const raw = await FlashRaw.open(copy);
    copy.fill(0);
    const f = await raw.decode();
    raw.close();
    assert.equal(fnv1a64U16(f.data), ref.hash_full);
  });

  test('supportsFile / detectFormat are cheap and correct', async () => {
    assert.equal(await supportsFile(buf), true);
    assert.equal(await detectFormat(buf), 'DNG');
    assert.equal(await detectFormat(file), 'DNG');
    assert.equal(await detectFormat(new Blob([buf])), 'DNG');
    assert.equal(await supportsFile(new Uint8Array(4096)), false);
    assert.equal(await supportsFile(new TextEncoder().encode('hello world, this is not a raw file')), false);
    assert.equal(await detectFormat(new Uint8Array(0)), null);
  });

  test('rejects unsupported input types', async () => {
    for (const bad of [42, null, undefined, {}, [1, 2, 3]]) {
      await assert.rejects(FlashRaw.open(bad), { kind: 'InvalidArgument' });
    }
  });
});

describe('backends', () => {
  const file = fixture('canon-5d3-mlv.dng');

  test('gpu falls back to CPU with a clear reason, or errors with fallback: "error"', async () => {
    const raw = await FlashRaw.openFile(file);
    try {
      const f = await raw.decode({ backend: 'gpu' });
      assert.equal(f.backend.requested, 'gpu');
      assert.equal(f.backend.kind, 'cpu');
      assert.match(f.backend.fallbackReason, /without the `gpu` feature/);
      assert.equal(fnv1a64U16(f.data), expected['canon-5d3-mlv.dng'].hash_full);
      const err = await raw.decode({ backend: 'webgpu', fallback: 'error' }).catch((e) => e);
      assert.ok(err instanceof GpuUnavailableError);
      assert.ok(err instanceof FlashRawError);
      assert.equal(err.kind, 'GpuUnavailable');
      await assert.rejects(raw.decode({ backend: 'gpu', fallback: 'maybe' }), { kind: 'InvalidArgument' });
      const simd = await raw.decode({ backend: 'cpu-simd' });
      assert.equal(simd.backend.requested, 'cpu-simd');
      assert.equal(simd.backend.fallbackReason, null);
    } finally {
      raw.close();
    }
  });
});

describe('zero-copy', () => {
  const file = fixture('canon-5d3-mlv.dng');
  const ref = expected['canon-5d3-mlv.dng'];

  test('zeroCopy frames view WASM memory until freed', async () => {
    const raw = await FlashRaw.openFile(file);
    const f = await raw.decode({ zeroCopy: true });
    assert.equal(f.zeroCopy, true);
    assert.equal(f.timings.copyMs, 0);
    assert.equal(fnv1a64U16(f.data), ref.hash_full);
    // Grow WASM memory with another big decode: the view must be re-derived, not detached.
    const other = await raw.decode({ output: 'raw-f32' });
    assert.equal(fnv1a64U16(f.data), ref.hash_full);
    assert.equal(other.data.length, f.data.length);
    const detached = f.detach();
    assert.equal(f.zeroCopy, false);
    assert.equal(fnv1a64U16(detached), ref.hash_full);
    const g = await raw.decode({ zeroCopy: true });
    g.free();
    assert.throws(() => g.data, { kind: 'Closed' });
    g.free(); // idempotent
    {
      using h = await raw.decode({ zeroCopy: true, output: 'rgba8' });
      assert.ok(h.data instanceof Uint8ClampedArray);
    }
    const p = await raw.preview({ zeroCopy: true });
    assert.deepEqual([...p.data.subarray(0, 2)], [0xff, 0xd8]);
    raw.close();
    assert.throws(() => p.data, { kind: 'Closed' });
  });
});
