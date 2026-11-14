// Worker-pool demo: each selected file is decoded (half-size RGBA8) in one of N workers; the
// main thread only draws. Files are passed by reference (structured clone of a File is cheap)
// and read inside the worker; decoded pixels come back as transferred buffers.
//
//   npm run build && node scripts/serve.mjs   →   http://127.0.0.1:8080/examples/worker/
//
// In your app: import { createWorkerPool } from 'flashraw/worker';
import { createWorkerPool } from '../../dist/worker.browser.js';

const size = document.getElementById('size');
size.value = Math.max(1, Math.min(4, (navigator.hardwareConcurrency || 2) - 1));
let pool = null;
let poolSize = 0;

document.getElementById('pick').addEventListener('change', async (e) => {
  const files = [...e.target.files];
  if (!files.length) return;
  if (!pool || poolSize !== Number(size.value)) {
    pool?.terminate();
    poolSize = Number(size.value);
    pool = createWorkerPool({ size: poolSize });
  }
  const grid = document.getElementById('grid');
  grid.replaceChildren();
  const t0 = performance.now();
  let done = 0;
  const summary = document.getElementById('summary');
  summary.textContent = `0 / ${files.length}`;
  await Promise.all(
    files.map(async (file) => {
      const fig = document.createElement('figure');
      const cap = document.createElement('figcaption');
      cap.textContent = `${file.name} — queued`;
      fig.append(cap);
      grid.append(fig);
      try {
        const frame = await pool.decode(file, { output: 'rgba8', demosaic: 'half-size' });
        const c = document.createElement('canvas');
        c.width = frame.width;
        c.height = frame.height;
        c.getContext('2d').putImageData(new ImageData(frame.data, frame.width, frame.height), 0, 0);
        fig.prepend(c);
        cap.textContent = `${file.name} — ${frame.width}×${frame.height}, ${frame.timings.totalMs.toFixed(0)} ms in worker`;
      } catch (err) {
        cap.textContent = `${file.name} — ${err.kind}: ${err.message}`;
      }
      summary.textContent = `${++done} / ${files.length} · ${(performance.now() - t0).toFixed(0)} ms wall · load ${pool.load.join(',')}`;
    }),
  );
});
