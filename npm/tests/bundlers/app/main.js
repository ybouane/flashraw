// Tiny app bundled by Vite / esbuild / webpack in tests/bundlers/run.mjs.
import { FlashRaw, getCapabilities } from 'flashraw';

function fnv1a64U16(data) {
  let lo = 0x84222325;
  let hi = 0xcbf29ce4;
  const step = (b) => {
    lo ^= b;
    const l = (lo >>> 0) * 0x1b3;
    const carry = Math.floor(l / 4294967296);
    hi = ((hi >>> 0) * 0x1b3 + carry + ((lo << 8) >>> 0)) >>> 0;
    lo = l >>> 0;
  };
  for (let i = 0; i < data.length; i++) {
    step(data[i] & 0xff);
    step(data[i] >>> 8);
  }
  return (hi >>> 0).toString(16).padStart(8, '0') + (lo >>> 0).toString(16).padStart(8, '0');
}

async function main() {
  const bytes = await (await fetch('./fixture.dng')).arrayBuffer();
  const raw = await FlashRaw.open(bytes);
  const frame = await raw.decode({ output: 'raw-u16' });
  const rgba = await raw.decode({ output: 'rgba8', demosaic: 'half-size' });
  raw.close();
  const caps = await getCapabilities();
  const out = {
    width: frame.width,
    height: frame.height,
    hash: fnv1a64U16(frame.data),
    rgba: [rgba.width, rgba.height, rgba.data.length],
    simd: caps.simd.active,
    binary: caps.wasm.binary,
    threads: caps.threads.count,
    threadsReason: caps.threads.reason,
    backend: frame.backend.decode,
  };
  // JPEG XL: the variant must not have been fetched so far, then load on the first JPEG XL DNG.
  const jxlFetches = () => performance.getEntriesByType('resource').map((e) => e.name.split('/').pop()).filter((n) => /jxl.*\.wasm$/.test(n));
  out.jxlBefore = jxlFetches();
  const jxl = await fetch('./jxl.dng');
  if (jxl.ok) {
    const r = await FlashRaw.open(await jxl.blob());
    const f = await r.decode();
    r.close();
    out.jxl = { hash: fnv1a64U16(f.data), binary: f.backend.binary, fetched: jxlFetches() };
  }
  if (globalThis.__withWorker) {
    const { createWorkerPool } = await import('flashraw/worker');
    const pool = createWorkerPool({ size: 1 });
    const w = await pool.decode(bytes);
    pool.terminate();
    out.workerHash = fnv1a64U16(w.data);
  }
  return out;
}

main().then(
  (r) => (window.__result = r),
  (e) => (window.__result = { error: String(e?.stack ?? e) }),
);
