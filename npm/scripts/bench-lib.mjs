// Benchmark kernel shared by Node (scripts/bench-child.mjs) and browsers (scripts/bench-page.html).
// `fr` is the flashraw module of the platform.

const median = (a) => {
  const s = [...a].sort((x, y) => x - y);
  return s.length % 2 ? s[s.length >> 1] : (s[s.length / 2 - 1] + s[s.length / 2]) / 2;
};

/** Named decode configurations. `gpu-*` need the WebGPU build (`init({ gpu: true })`). */
export const CASES = {
  'raw-u16': { output: 'raw-u16' },
  rgba8: { output: 'rgba8' },
  // WebGPU, pixels read back into a typed array (comparable to `rgba8`).
  'gpu-rgba8-readback': { backend: 'gpu', output: 'rgba8', fallback: 'error' },
  // WebGPU, result left in a GPUBuffer (wall time includes waiting for the GPU to finish).
  'gpu-rgba8-resident': { output: 'gpu-rgba8', fallback: 'error' },
};

export async function benchFile(fr, name, bytes, { iterations = 7, warmup = 2, cases = ['raw-u16', 'rgba8'] } = {}) {
  const out = { file: name, bytes: bytes.length };
  const opens = [];
  let raw;
  for (let i = 0; i < warmup + iterations; i++) {
    const t = performance.now();
    raw?.close();
    raw = await fr.FlashRaw.open(bytes);
    if (i >= warmup) opens.push(performance.now() - t);
  }
  out.camera = raw.metadata.camera.name;
  out.compression = raw.metadata.compression;
  out.megapixels = (raw.frameInfo.width * raw.frameInfo.height) / 1e6;
  out.openMs = median(opens);
  for (const key of cases) {
    const wall = [];
    const inner = [];
    let ok = true;
    let backend = null;
    for (let i = 0; i < warmup + iterations; i++) {
      let f;
      const t = performance.now();
      try {
        f = await raw.decode(CASES[key]);
      } catch (e) {
        out[key] = { error: e.kind ?? String(e), message: String(e?.message ?? e) };
        ok = false;
        break;
      }
      const ms = performance.now() - t;
      backend = f.backend;
      if (i >= warmup) {
        wall.push(f.gpuResident ? ms : f.timings.wallMs);
        inner.push(f.timings.totalMs);
      }
      f.free?.();
    }
    if (ok) {
      out[key] = {
        wallMs: median(wall),
        decodeMs: median(inner),
        mpPerS: out.megapixels / (median(wall) / 1000),
        backend: backend?.decode,
        postprocess: backend?.postprocess ?? null,
      };
    }
  }
  raw.close();
  return out;
}
