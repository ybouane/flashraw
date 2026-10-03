#!/usr/bin/env node
// Regenerate tests/fixtures/expected.json from the native CLI (`flashraw verify --json
// --apply-opcodes`: hashes of the default decode, DNG opcode lists applied).
// The Node tests compare against the live CLI when it is available and fall back to this file.
import { writeFileSync } from 'node:fs';
import { basename, join } from 'node:path';
import { fixtures, fixturesDir, nativeVerify } from './helpers.mjs';

const out = {};
for (const f of fixtures) {
  const v = nativeVerify(f);
  if (!v) throw new Error('native CLI unavailable; build it with `cargo build --release -p flashraw-cli`');
  out[basename(f)] = {
    format: v.format,
    make: v.canonical_make,
    model: v.canonical_model,
    width: v.raw_width,
    height: v.raw_height,
    cpp: v.cpp,
    cfa: v.cfa,
    white: v.white,
    hash_full: v.hash_full,
    previews: v.previews,
  };
}
writeFileSync(join(fixturesDir, 'expected.json'), JSON.stringify(out, null, 2) + '\n');
console.log(out);
