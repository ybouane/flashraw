// Child process for threads.test.mjs / simd.test.mjs: prints the loaded binary and output hashes
// as JSON. The binary is chosen by the environment (FLASHRAW_WASM_THREADS, FLASHRAW_WASM_SIMD).
import { createHash } from 'node:crypto';
import { FlashRaw, init, getCapabilities } from 'flashraw';
import { fnv1a64U16 } from './helpers.mjs';

const info = await init();
const caps = await getCapabilities();
const hashes = {};
const ms = {};
for (const file of process.argv.slice(2)) {
  const raw = await FlashRaw.openFile(file);
  const h = {};
  const t = {};
  for (const [key, opts] of [
    ['u16', { output: 'raw-u16' }],
    ['rgba8', { output: 'rgba8' }],
    ['half', { output: 'rgba8', demosaic: 'half-size' }],
    ['linear', { output: 'linear-rgb' }],
  ]) {
    try {
      const f = await raw.decode(opts);
      // raw-u16: the native CLI's FNV-1a; other outputs: SHA-1 of the bytes (much faster in JS).
      h[key] =
        key === 'u16'
          ? fnv1a64U16(f.data)
          : createHash('sha1').update(new Uint8Array(f.data.buffer, f.data.byteOffset, f.data.byteLength)).digest('hex');
      t[key] = f.timings.wallMs;
    } catch (e) {
      h[key] = `error:${e.kind}`;
    }
  }
  if (raw.tiles.length > 1) {
    const f = await raw.decodeTile(raw.tiles.length - 1);
    h.lastTile = fnv1a64U16(f.data);
  }
  raw.close();
  hashes[file] = h;
  ms[file] = t;
}
process.stdout.write(
  JSON.stringify({
    variant: info.variant,
    binary: info.binary,
    level: info.simdLevel,
    threads: caps.threads.count,
    hashes,
    ms,
  }),
);
