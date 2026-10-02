// Benchmark kernel shared by Node (scripts/bench-child.mjs) and Chromium (scripts/bench-page.html).
// `fr` is the flashraw module of the platform.

const median = (a) => {
  const s = [...a].sort((x, y) => x - y);
  return s.length % 2 ? s[s.length >> 1] : (s[s.length / 2 - 1] + s[s.length / 2]) / 2;
};

export async function benchFile(fr, name, bytes, { iterations = 7, warmup = 2 } = {}) {
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
  for (const output of ['raw-u16', 'rgba8']) {
    const wall = [];
    const inner = [];
    let ok = true;
    for (let i = 0; i < warmup + iterations; i++) {
      let f;
      try {
        f = await raw.decode({ output });
      } catch (e) {
        out[output] = { error: e.kind ?? String(e) };
        ok = false;
        break;
      }
      if (i >= warmup) {
        wall.push(f.timings.wallMs);
        inner.push(f.timings.totalMs);
      }
    }
    if (ok) out[output] = { wallMs: median(wall), decodeMs: median(inner), mpPerS: out.megapixels / (median(wall) / 1000) };
  }
  raw.close();
  return out;
}
