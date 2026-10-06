import { test, describe } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import {
  FlashRaw,
  decodeRaw,
  readMetadata,
  FlashRawError,
  UnsupportedFormatError,
  ClosedError,
  IoError,
  InvalidArgumentError,
  ERROR_KINDS,
  errorFromKind,
} from 'flashraw';
import { fixture, fixtures, corpusFile } from './helpers.mjs';

/** Run `fn`; it may succeed, but any failure must be a structured, non-internal FlashRawError. */
async function noCrash(fn, label) {
  try {
    await fn();
    return 'ok';
  } catch (e) {
    assert.ok(e instanceof FlashRawError, `${label}: expected FlashRawError, got ${e?.stack ?? e}`);
    assert.ok(ERROR_KINDS.includes(e.kind), `${label}: unknown kind ${e.kind}`);
    assert.notEqual(e.kind, 'Internal', `${label}: WASM trap / panic: ${e.message}`);
    assert.equal(typeof e.message, 'string');
    return e.kind;
  }
}

describe('structured errors', () => {
  test('garbage input → UnsupportedFormatError', async () => {
    const e = await FlashRaw.open(new Uint8Array(4096).fill(0x5a)).catch((x) => x);
    assert.ok(e instanceof UnsupportedFormatError);
    assert.ok(e instanceof FlashRawError);
    assert.ok(e instanceof Error);
    assert.equal(e.kind, 'UnsupportedFormat');
    assert.equal(e.name, 'UnsupportedFormatError');
    assert.deepEqual(JSON.parse(JSON.stringify(e)), { name: e.name, kind: e.kind, message: e.message });
    await assert.rejects(FlashRaw.open(new Uint8Array(0)), { kind: 'UnsupportedFormat' });
    await assert.rejects(readMetadata(new Uint8Array([0x49, 0x49, 0x2a, 0x00])), FlashRawError);
  });

  test('bogus TIFF headers → structured errors', async () => {
    const t = new Uint8Array(256);
    t.set([0x49, 0x49, 0x2a, 0x00, 0xff, 0xff, 0xff, 0x7f]); // IFD offset far beyond EOF
    assert.notEqual(await noCrash(() => decodeRaw(t), 'tiff-oob'), 'ok');
    const m = new Uint8Array(256);
    m.set([0x4d, 0x4d, 0x00, 0x2a, 0, 0, 0, 8, 0xff, 0xff]); // 65535 IFD entries
    assert.notEqual(await noCrash(() => decodeRaw(m), 'tiff-many-entries'), 'ok');
  });

  for (const file of fixtures) {
    test(`truncated ${file.split('/').pop()} never crashes`, async () => {
      const bytes = readFileSync(file);
      const kinds = new Set();
      for (const frac of [0.001, 0.01, 0.1, 0.3, 0.5, 0.7, 0.9, 0.99]) {
        const cut = bytes.subarray(0, Math.max(8, Math.floor(bytes.length * frac)));
        kinds.add(
          await noCrash(async () => {
            const raw = await FlashRaw.open(cut);
            try {
              raw.metadata;
              await raw.preview();
              // Recovered by default: a truncated payload is reported on the frame.
              if ((await raw.decode()).incomplete) kinds.add('incomplete');
            } finally {
              raw.close();
            }
          }, `cut ${frac}`),
        );
      }
      assert.ok([...kinds].some((k) => k !== 'ok'), 'some truncation must be reported (error or incomplete frame)');
    });
  }

  test('random corruption never crashes', async () => {
    const bytes = readFileSync(fixture('canon-5d3-mlv.dng'));
    let seed = 12345;
    const rnd = () => ((seed = (seed * 1103515245 + 12345) >>> 0) / 4294967296);
    for (let round = 0; round < 24; round++) {
      const c = new Uint8Array(bytes);
      // Corrupt both header (first 16 KiB) and payload bytes.
      for (let i = 0; i < 40; i++) {
        const pos = i % 2 ? Math.floor(rnd() * 16384) : Math.floor(rnd() * c.length);
        c[pos] = Math.floor(rnd() * 256);
      }
      await noCrash(async () => {
        const raw = await FlashRaw.open(c);
        try {
          await raw.decode({ output: round % 3 === 0 ? 'rgba8' : 'raw-u16', demosaic: 'half-size' });
        } finally {
          raw.close();
        }
      }, `round ${round}`);
    }
  });

  test('invalid options → InvalidArgumentError', async () => {
    const raw = await FlashRaw.openFile(fixture('canon-5d3-mlv.dng'));
    try {
      for (const opts of [
        { output: 'png' },
        { backend: 'tpu' },
        { demosaic: 'ahd' },
        { crop: 'yes' },
        { region: { x: -1, y: 0, width: 10, height: 10 } },
        { region: { x: 0, y: 0, width: 0, height: 10 } },
        { limits: { maxSamples: -5 } },
      ]) {
        const e = await raw.decode(opts).catch((x) => x);
        assert.ok(e instanceof InvalidArgumentError, `${JSON.stringify(opts)} → ${e}`);
      }
      await assert.rejects(raw.decodeRegion({ x: 99999, y: 0, width: 10, height: 10 }), { kind: 'InvalidArgument' });
      await assert.rejects(raw.decode('rgba8'), { kind: 'InvalidArgument' });
      await assert.rejects(raw.preview({ index: 99 }), { kind: 'NotAvailable' });
      await assert.rejects(raw.preview({ index: -1 }), { kind: 'InvalidArgument' });
      await assert.rejects(raw.preview({ format: 'webp' }), { kind: 'InvalidArgument' });
      // Still usable after argument errors.
      assert.equal((await raw.decode()).width, 1920);
    } finally {
      raw.close();
    }
  });

  test('limits → LimitExceededError', async () => {
    await assert.rejects(decodeRaw(fixture('canon-5d3-mlv.dng'), { limits: { maxSamples: 1000 } }), {
      kind: 'LimitExceeded',
    });
    await assert.rejects(decodeRaw(fixture('canon-5d3-mlv.dng'), { limits: { maxDimension: 100 } }), {
      kind: 'LimitExceeded',
    });
  });

  test('closed handles → ClosedError', async () => {
    const raw = await FlashRaw.openFile(fixture('canon-5d3-mlv.dng'));
    raw.close();
    raw.close(); // idempotent
    assert.equal(raw.closed, true);
    assert.throws(() => raw.metadata, ClosedError);
    await assert.rejects(raw.decode(), ClosedError);
    await assert.rejects(raw.preview(), ClosedError);
  });

  test('missing files → IoError', async () => {
    const e = await FlashRaw.openFile('/nonexistent/file.dng').catch((x) => x);
    assert.ok(e instanceof IoError);
    assert.match(e.message, /ENOENT/);
    await assert.rejects(FlashRaw.open('/nonexistent/x.dng'), { kind: 'Io' });
  });

  test('errorFromKind maps every kind to its subclass', () => {
    for (const k of ERROR_KINDS) {
      const e = errorFromKind(k, 'm');
      assert.equal(e.kind, k);
      assert.equal(e.name, `${k}Error`);
      assert.ok(e instanceof FlashRawError);
    }
    assert.equal(errorFromKind('Weird', 'x').kind, 'Weird');
  });

  test('the constructor is private', () => {
    assert.throws(() => new FlashRaw(), TypeError);
  });
});

describe('truncated entropy streams', () => {
  const nef = corpusFile('Nikon/Z 7/3-Nikon-Z7-RAW-14bit-lossless-compressed-L.NEF');
  test('truncated Nikon Huffman NEF → recovered rows + warning, CorruptFileError when strict (never silent)', { skip: !nef && 'corpus not available' }, async () => {
    const bytes = readFileSync(nef);
    for (const pct of [50, 90, 99]) {
      const cut = bytes.subarray(0, Math.floor((bytes.length * pct) / 100));
      const e = await decodeRaw(cut, { corruptionPolicy: 'strict' }).then(() => null, (x) => x);
      assert.ok(e, `${pct} %: decoded a truncated file`);
      assert.equal(e.kind, 'CorruptFile', `${pct} %: ${e.message}`);
      assert.match(e.message, /truncated/);
      const f = await decodeRaw(cut);
      assert.equal(f.incomplete, true, `${pct} %`);
      assert.equal(f.warnings[0].kind, 'truncated');
      assert.ok(f.warnings[0].samplesFilled > 0);
    }
    const ok = await decodeRaw(bytes);
    assert.equal(ok.width * ok.height, ok.data.length);
  });
});
