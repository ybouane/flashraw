// FlashRaw browser demo: drop a RAW file → metadata + embedded preview instantly → full decode
// (RGBA8) on a canvas, with timings. Serve the npm/ directory:
//
//   npm run build && node scripts/serve.mjs   →   http://127.0.0.1:8080/examples/browser/
//
// In your app, import from 'flashraw' and 'flashraw/worker' instead of the dist paths.
import { FlashRaw, getCapabilities, supportsFile } from '../../dist/browser.js';
import { createWorkerPool } from '../../dist/worker.browser.js';

const $ = (id) => document.getElementById(id);
const drop = $('drop');
const status = (s) => ($('status').textContent = s);
let pool = null;

const caps = await getCapabilities();
$('caps').textContent =
  `v${caps.version} · WASM ${caps.wasm.variant} (${caps.simd.level}) · init ${caps.wasm.initMs.toFixed(1)} ms · ` +
  `${caps.threads.hardwareConcurrency} cores · WebGPU ${caps.gpu.webgpu ? 'present' : 'absent'}${caps.gpu.compiled ? '' : ' (GPU backend not built)'}`;

drop.addEventListener('dragover', (e) => {
  e.preventDefault();
  drop.classList.add('over');
});
drop.addEventListener('dragleave', () => drop.classList.remove('over'));
drop.addEventListener('drop', (e) => {
  e.preventDefault();
  drop.classList.remove('over');
  const f = e.dataTransfer.files[0];
  if (f) load(f);
});
$('pick').addEventListener('change', (e) => e.target.files[0] && load(e.target.files[0]));

function row(k, v) {
  if (v === null || v === undefined || v === '') return '';
  return `<tr><td>${k}</td><td>${String(v).replace(/</g, '&lt;')}</td></tr>`;
}

function showMetadata(m, extra) {
  const e = m.exposure;
  const shutter = e.exposureTime ? (e.exposureTime < 1 ? `1/${Math.round(1 / e.exposureTime)} s` : `${e.exposureTime} s`) : null;
  $('meta').innerHTML = `
    <h2>Camera</h2><table>
      ${row('Camera', m.camera.name)}${row('Lens', m.lens.model)}${row('Captured', m.captureTime)}
      ${row('ISO', e.iso)}${row('Shutter', shutter)}${row('Aperture', e.fNumber && `f/${e.fNumber}`)}
      ${row('Focal length', e.focalLength && `${e.focalLength} mm`)}
    </table>
    <h2>Image</h2><table>
      ${row('Format', `${m.format}${m.dngVersion ? ` ${m.dngVersion}` : ''}`)}${row('Compression', m.compression)}
      ${row('Size', `${m.width} × ${m.height} (sensor ${m.rawWidth} × ${m.rawHeight})`)}
      ${row('Bits', m.bitsPerSample)}${row('CFA', m.cfa?.pattern ?? m.layout)}
      ${row('Black / white', `${m.blackLevel.mean.toFixed(1)} / ${m.whiteLevel.join(', ')}`)}
      ${row('WB (as shot)', m.whiteBalance?.map((x) => x.toFixed(3)).join(' '))}
      ${row('Orientation', m.orientation)}${row('Previews', m.previews.map((p) => `${p.kind} ${p.width}×${p.height} ${p.encoding}`).join(', '))}
    </table>
    <h2>Timings</h2><table>${Object.entries(extra).map(([k, v]) => row(k, v)).join('')}</table>`;
}

async function showPreview(p) {
  // Embedded JPEGs are decoded by the browser (fast, native); other encodings come as RGBA.
  const bmp =
    p.format === 'rgba8'
      ? await createImageBitmap(new ImageData(p.data, p.width, p.height))
      : await createImageBitmap(new Blob([p.data], { type: p.mimeType }), { imageOrientation: 'from-image' });
  const c = document.createElement('canvas');
  c.width = bmp.width;
  c.height = bmp.height;
  c.getContext('2d').drawImage(bmp, 0, 0);
  return c;
}

async function load(file) {
  const t = { start: performance.now() };
  const timings = {};
  drop.innerHTML = '<p>Reading…</p>';
  if (!(await supportsFile(file))) {
    drop.innerHTML = `<p>${file.name}: not a supported RAW file.</p>`;
    return;
  }
  const useWorker = $('useWorker').checked;
  // Worker mode: the File is passed by reference and read inside the worker (no main-thread copy).
  pool ??= useWorker ? createWorkerPool({ size: 1 }) : null;
  let raw;
  try {
    raw = useWorker ? await pool.open(file) : await FlashRaw.open(file);
    timings['Open + metadata'] = `${(performance.now() - t.start).toFixed(1)} ms (parse ${raw.parseMs.toFixed(2)} ms)`;
    showMetadata(raw.metadata, timings);

    const p0 = performance.now();
    let prev = await raw.preview();
    if (prev && prev.mimeType !== 'image/jpeg') prev = await raw.preview({ format: 'rgba8' });
    if (prev) {
      const c = await showPreview(prev);
      drop.replaceChildren(stage(c, `Embedded preview ${prev.width}×${prev.height}`));
      timings['Preview'] = `${(performance.now() - p0).toFixed(1)} ms (${(prev.data.length / 1024).toFixed(0)} KiB ${prev.mimeType})`;
      showMetadata(raw.metadata, timings);
    } else {
      drop.innerHTML = '<p>No embedded preview — decoding sensor data…</p>';
    }

    status('Decoding…');
    const frame = await raw.decode({ output: 'rgba8', demosaic: $('demosaic').value });
    const c = document.createElement('canvas');
    c.width = frame.width;
    c.height = frame.height;
    c.getContext('2d').putImageData(new ImageData(frame.data, frame.width, frame.height), 0, 0);
    drop.replaceChildren(stage(c, `Decoded RAW ${frame.width}×${frame.height} · ${frame.backend.decode}`));
    timings['Decode → RGBA8'] = `${frame.timings.wallMs.toFixed(1)} ms`;
    for (const [k, v] of Object.entries(frame.timings.stages)) timings[`  ${k}`] = `${v.toFixed(1)} ms`;
    timings['Backend'] = frame.backend.decode + (frame.backend.fallbackReason ? ` (fallback: ${frame.backend.fallbackReason})` : '');
    timings['Total'] = `${(performance.now() - t.start).toFixed(1)} ms`;
    showMetadata(raw.metadata, timings);
    status('');
  } catch (e) {
    drop.innerHTML = `<p>${e.name} (${e.kind ?? 'error'}): ${e.message}</p>`;
    status('');
  } finally {
    await raw?.close();
  }
}

function stage(canvas, caption) {
  const s = document.createElement('div');
  s.className = 'stage';
  const cap = document.createElement('div');
  cap.className = 'caption';
  cap.textContent = caption;
  s.append(canvas, cap);
  return s;
}
