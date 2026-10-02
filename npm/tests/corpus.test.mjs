// Larger samples from the CC0 corpus (raw.pixls.us). Skipped when $FLASHRAW_CORPUS (or the
// default corpus location) is absent. Compares WASM output against the native CLI.
import { test, describe } from 'node:test';
import assert from 'node:assert/strict';
import { FlashRaw, FlashRawError } from 'flashraw';
import { corpusRoot, corpusFile, fnv1a64U16, nativeVerify, cliBinary } from './helpers.mjs';

const SAMPLES = [
  'Leica/M8/L1030132.DNG',
  'Pentax/K-50/IMGP1385.DNG',
  'PENTAX/PENTAX K-r/IMGP4425.DNG',
  'Ricoh/GR/R0003611.DNG',
  'Apple/iPhone XS/IMG_1105.dng',
  'Apple/iPhone 8/RAW_2018_11_07_14_43_14_820_noflash.dng',
  'Google/Pixel 6 Pro/PXL_20220910_093206982.dng',
  'Xiaomi/MI 8/IMG_20200101_174326.dng',
  'Sigma/fp/A002_651_20240229_000001_2k_12bit.DNG',
  'DJI/FC4382/DJI_20250506184749_0011_D.DNG',
  'Blackmagic/Micro Cinema Camera/CAM1_2000-01-01_1707_C0003_000100.dng',
];

describe('corpus DNGs vs native CLI', { skip: !corpusRoot || !cliBinary() ? 'corpus or native CLI unavailable' : false }, () => {
  for (const rel of SAMPLES) {
    const file = corpusFile(rel);
    test(rel, { skip: !file && 'not in corpus' }, async () => {
      const ref = nativeVerify(file);
      let raw;
      try {
        raw = await FlashRaw.openFile(file);
      } catch (e) {
        assert.ok(e instanceof FlashRawError);
        assert.equal(e.kind, ref.error_kind, `native: ${ref.error}`);
        return;
      }
      try {
        const frame = await raw.decode().catch((e) => e);
        if (ref.error_kind) {
          // Unsupported by the native decoder too (e.g. lossy cinema DNG): same structured kind.
          assert.ok(frame instanceof FlashRawError, 'native failed, WASM must fail too');
          assert.equal(frame.kind, ref.error_kind);
          return;
        }
        assert.equal(raw.metadata.camera.canonicalMake, ref.canonical_make);
        assert.equal(raw.metadata.camera.canonicalModel, ref.canonical_model);
        assert.equal(frame.width, ref.raw_width);
        assert.equal(frame.height, ref.raw_height);
        assert.equal(frame.cfa?.pattern ?? null, ref.cfa);
        assert.equal(fnv1a64U16(frame.data), ref.hash_full);
        const p = await raw.preview();
        assert.equal(p === null, ref.previews === 0);
      } finally {
        raw.close();
      }
    });
  }
});
