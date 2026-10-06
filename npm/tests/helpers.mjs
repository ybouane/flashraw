// Shared test helpers.
import { existsSync, readdirSync, statSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { homedir } from 'node:os';

export const here = dirname(fileURLToPath(import.meta.url));
export const pkgDir = resolve(here, '..');
export const repoRoot = resolve(pkgDir, '..');
export const fixturesDir = join(here, 'fixtures');

/** Bundled CC0 fixtures (from raw.pixls.us). */
export const fixtures = readdirSync(fixturesDir)
  .filter((f) => /\.dng$/i.test(f))
  .map((f) => join(fixturesDir, f));

export const fixture = (name) => join(fixturesDir, name);

/** Corpus root, or null when unavailable (corpus tests are skipped then). */
export const corpusRoot = (() => {
  const c = process.env.FLASHRAW_CORPUS ?? join(repoRoot, '..', '..', '..', 'corpus');
  for (const p of [process.env.FLASHRAW_CORPUS, c, '/var/projects/flashraw/corpus']) {
    if (p && existsSync(p) && statSync(p).isDirectory()) return p;
  }
  return null;
})();

export function corpusFile(rel) {
  if (!corpusRoot) return null;
  const p = join(corpusRoot, rel);
  return existsSync(p) ? p : null;
}

/** Real-world samples outside the corpus ($FLASHRAW_REALWORLD), or null when unavailable. */
export const realworldRoot = (() => {
  for (const p of [process.env.FLASHRAW_REALWORLD, join(repoRoot, '..', 'flashraw-realworld'), '/var/projects/flashraw-realworld']) {
    if (p && existsSync(p) && statSync(p).isDirectory()) return p;
  }
  return null;
})();

export function realworldFile(rel) {
  if (!realworldRoot) return null;
  const p = join(realworldRoot, rel);
  return existsSync(p) ? p : null;
}

/** FNV-1a 64 over the little-endian bytes of u16 samples — same as `flashraw verify` hash_full. */
export function fnv1a64U16(data) {
  // 64-bit state as two 32-bit halves; prime = 2^40 + 0x1b3.
  let lo = 0x84222325;
  let hi = 0xcbf29ce4;
  const step = (byte) => {
    lo ^= byte;
    const l = (lo >>> 0) * 0x1b3;
    const carry = Math.floor(l / 4294967296);
    const nh = ((hi >>> 0) * 0x1b3 + carry + ((lo << 8) >>> 0)) >>> 0;
    lo = l >>> 0;
    hi = nh;
  };
  for (let i = 0; i < data.length; i++) {
    const v = data[i];
    step(v & 0xff);
    step(v >>> 8);
  }
  return (hi >>> 0).toString(16).padStart(8, '0') + (lo >>> 0).toString(16).padStart(8, '0');
}

/** The native CLI binary, or null. */
export function cliBinary() {
  const candidates = [process.env.FLASHRAW_CLI, join(repoRoot, 'target/release/flashraw')];
  for (const c of candidates) if (c && existsSync(c)) return c;
  return null;
}

/** `flashraw verify --json` for a file via the native CLI (null when the CLI is unavailable). */
export function nativeVerify(file) {
  const bin = cliBinary();
  if (bin) {
    // Exits non-zero for files it cannot decode, but still prints a JSON record with `error_kind`.
    // `--apply-opcodes`: hash what a default decode returns (DNG opcode lists applied).
    const r = spawnSync(bin, ['verify', '--json', '--apply-opcodes', file], { encoding: 'utf8' });
    return JSON.parse(r.stdout);
  }
  if (process.env.FLASHRAW_NO_CARGO) return null;
  const cargo = process.env.CARGO ?? join(homedir(), '.cargo/bin/cargo');
  const r = spawnSync(cargo, ['run', '-q', '--release', '-p', 'flashraw-cli', '--', 'verify', '--json', '--apply-opcodes', file], {
    cwd: repoRoot,
    encoding: 'utf8',
  });
  return r.status === 0 ? JSON.parse(r.stdout) : null;
}
