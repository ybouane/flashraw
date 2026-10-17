// The demo: open a RAW file with FlashRaw (WASM), show its metadata and embedded preview,
// decode it once to linear light (`linear-rgb-f16`) and develop it live on the GPU.

import {
  FlashRaw,
  FlashRawError,
  NotAvailableError,
  getCapabilities,
  init,
  reinit,
  type BuildInfo,
  type Capabilities,
  type DecodedFrame,
  type InitOptions,
  type PreviewDescriptor,
  type RawMetadata,
} from 'flashraw';
import { DEFAULTS, LOOK, SLIDERS, isDefault, orientedSize, uniforms, type Adjustments, type Uniforms } from './pipeline';
import { emptyHistogram, type Histogram, type Renderer, type Source } from './renderer';
import { GlRenderer } from './renderer-gl';
import { CpuRenderer } from './renderer-cpu';
import { Viewport } from './viewport';
import { bytes, h, renderMetadata } from './metadata';

const $ = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;
const params = new URLSearchParams(location.search);
const now = () => performance.now();
const ms = (v: number | null | undefined) => (v == null ? '–' : v < 10 ? v.toFixed(2) : v < 100 ? v.toFixed(1) : Math.round(v).toString());

// ------------------------------------------------------------------- engine (lazy)
//
// 'auto': the WebGPU build with the GPU backend when the browser has WebGPU and an adapter,
// otherwise the threaded build (cross-origin isolated pages) or single-threaded SIMD. The build
// can be switched at any time (`reinit`): the open file is re-opened with the new one.

type Engine = 'auto' | 'gpu' | 'threads' | 'single' | 'scalar';
const ENGINES: Engine[] = ['auto', 'gpu', 'threads', 'single', 'scalar'];
const asEngine = (v: string | null): Engine => (ENGINES.includes(v as Engine) ? (v as Engine) : 'auto');
let engine: Engine = asEngine(params.get('engine'));
/** Why 'auto' chose the build it did (shown in the performance panel). */
let engineNote = '';

function optionsFor(e: Exclude<Engine, 'auto'>): InitOptions {
  switch (e) {
    case 'gpu':
      return { gpu: true };
    case 'single':
      return { threads: false };
    case 'scalar':
      return { simd: false, threads: false };
    default:
      return {};
  }
}

interface GpuLike {
  requestAdapter(): Promise<unknown>;
}

/** null when WebGPU has an adapter here, otherwise why not. */
async function webgpuUnavailable(): Promise<string | null> {
  const gpu = (navigator as Navigator & { gpu?: GpuLike }).gpu;
  if (!gpu) return 'this browser has no WebGPU';
  try {
    return (await gpu.requestAdapter()) ? null : 'WebGPU found no GPU adapter';
  } catch (e) {
    return `WebGPU adapter request failed: ${(e as Error).message}`;
  }
}

let selected = false;
/** The first selection is `init()`, later ones `reinit()` (engines are reused, never reloaded). */
function select(o: InitOptions) {
  const first = !selected;
  selected = true;
  return first ? init(o) : reinit(o);
}

async function resolveEngine(e: Engine): Promise<BuildInfo> {
  engineNote = '';
  if (e !== 'auto') {
    const b = await select(optionsFor(e));
    caps = await getCapabilities();
    return b;
  }
  let why = await webgpuUnavailable();
  if (!why) {
    const b = await select({ gpu: true });
    if (b.binary === 'gpu') {
      caps = await getCapabilities(); // requests FlashRaw's WebGPU device
      if (caps.gpu.available) {
        engineNote = 'Auto: WebGPU is available, so the WebGPU build with the GPU backend';
        return b;
      }
      why = caps.gpu.reason ?? 'no WebGPU device';
    } else why = b.gpuReason ?? 'the WebGPU build did not load';
  }
  const b = await select({});
  caps = await getCapabilities();
  engineNote = `Auto: the CPU build, because ${why}`;
  return b;
}

let initPromise: Promise<BuildInfo> | null = null;
let initWall = 0;
function startEngine(e: Engine) {
  const t = now();
  const prev = initPromise;
  const p = (prev ? prev.catch(() => null) : Promise.resolve(null)).then(() => resolveEngine(e)).then((b) => {
    initWall = now() - t;
    return b;
  });
  initPromise = p;
  p.catch(() => {
    if (initPromise === p) initPromise = null;
  });
  return p;
}
export function warmUp() {
  return initPromise ?? startEngine(engine);
}

// ------------------------------------------------------------------- state

interface PreviewState {
  desc: PreviewDescriptor;
  blob: Blob;
  bitmap: ImageBitmap | null;
  /** orientation still to apply to `bitmap` */
  orientation: number;
  extractMs: number;
  decodeMs: number;
}

interface FileTimings {
  openMs: number;
  parseMs: number;
  metaMs: number;
  preview: PreviewState | null;
  decodeMs?: number;
  wallMs?: number;
  uploadMs?: number;
  firstPixelsMs?: number;
  stages?: Record<string, number>;
  copyMs?: number;
}

const ui = {
  status: $('status'),
  dropzone: $('dropzone'),
  workspace: $('workspace'),
  stage: $('stage'),
  overlay: $('stage-overlay'),
  overlayMsg: $('stage-message'),
  hint: $('stage-hint'),
  zoomLevel: $('zoom-level'),
  fileLabel: $('file-label'),
  dims: $('image-dims'),
  srcRaw: $<HTMLButtonElement>('src-raw'),
  srcPreview: $<HTMLButtonElement>('src-preview'),
  hist: $<HTMLCanvasElement>('histogram'),
  clipInfo: $('clip-info'),
  sliders: $('sliders'),
  tone: $<HTMLSelectElement>('tone'),
  autoAdjust: $<HTMLButtonElement>('auto-adjust'),
  renderInfo: $('render-info'),
  dlPng: $<HTMLButtonElement>('dl-png'),
  dlJpeg: $<HTMLButtonElement>('dl-jpeg'),
  dlPreview: $<HTMLButtonElement>('dl-preview'),
  dlInfo: $('dl-info'),
  demosaic: $<HTMLSelectElement>('opt-demosaic'),
  backend: $<HTMLSelectElement>('opt-backend'),
  engine: $<HTMLSelectElement>('opt-engine'),
  redecode: $<HTMLButtonElement>('redecode'),
  compare: $<HTMLButtonElement>('compare'),
  perfTiles: $('perf-tiles'),
  perfStages: $('perf-stages'),
  perfBackend: $('perf-backend'),
  perfEngine: $('perf-engine'),
  metaGrid: $('meta-grid'),
  previewsCard: $('previews-card'),
  metaJson: $('meta-json'),
  copyJson: $<HTMLButtonElement>('copy-json'),
};

let renderer: Renderer | null = null;
let viewport: Viewport | null = null;
let raw: FlashRaw | null = null;
let currentFile: File | null = null;
let meta: RawMetadata | null = null;
let timings: FileTimings | null = null;
let token = 0;
let busy = false;
let hasRaw = false;
let rawSize: [number, number] = [0, 0]; // displayed (oriented) size of the decode
let source: Source = 'raw';
let compare = false;
let adjust: Adjustments = { ...DEFAULTS };
/** the file's own exposure offset (DNG BaselineExposure), used by the film look */
let baseline = 0;
let caps: Capabilities | null = null;
let lastFrameInfo: { width: number; height: number; demosaic: string; backend: DecodedFrame['backend']; warnings: number; incomplete: boolean } | null = null;

function setStatus(text: string, error = false) {
  ui.status.textContent = text;
  ui.status.classList.toggle('error', error);
}

function overlay(text: string | null) {
  ui.overlay.hidden = text === null;
  if (text !== null) ui.overlayMsg.textContent = text;
}

// ------------------------------------------------------------------- renderer + viewer

function setupViewer() {
  if (renderer) return;
  const forceCpu = params.get('renderer') === 'cpu';
  renderer = forceCpu ? null : GlRenderer.create();
  if (!renderer) {
    renderer = new CpuRenderer();
    ui.renderInfo.textContent = `${forceCpu ? 'CPU renderer selected' : 'WebGL2 is unavailable'}: adjustments run in JavaScript (screen preview at reduced resolution; downloads are full resolution).`;
  }
  if (renderer instanceof GlRenderer) {
    renderer.onLost = () => setStatus('The GPU context was lost; open the file again to continue.', true);
  }
  ui.stage.prepend(renderer.canvas);
  viewport = new Viewport(ui.stage);
  viewport.onChange = () => {
    ui.zoomLevel.textContent = `${Math.round(viewport!.view.scale * 100)}%`;
    requestRender();
  };
  viewport.onInteract = () => ui.hint.classList.add('gone');

  $('zoom-in').addEventListener('click', () => viewport!.zoomBy(1.25));
  $('zoom-out').addEventListener('click', () => viewport!.zoomBy(0.8));
  $('zoom-fit').addEventListener('click', () => viewport!.fit());
  $('zoom-1').addEventListener('click', () => viewport!.zoomTo(1));
  ui.compare.addEventListener('click', () => setCompare(!compare));
  ui.stage.addEventListener('keydown', (e) => {
    if ((e.key === 'b' || e.key === 'B') && !e.repeat) setCompare(true);
  });
  ui.stage.addEventListener('keyup', (e) => {
    if (e.key === 'b' || e.key === 'B') setCompare(false);
  });
  for (const b of [ui.srcRaw, ui.srcPreview]) {
    b.addEventListener('click', () => setSource(b.dataset.source as Source));
  }
  buildSliders();
  ui.tone.addEventListener('change', () => {
    adjust.tone = Number(ui.tone.value) as 0 | 1;
    adjustmentsChanged();
  });
  ui.autoAdjust.addEventListener('click', autoExposure);
  $('reset-adjust').addEventListener('click', () => {
    adjust = { ...DEFAULTS };
    syncSliders();
    adjustmentsChanged();
  });
  ui.dlPng.addEventListener('click', () => void download('image/png'));
  ui.dlJpeg.addEventListener('click', () => void download('image/jpeg'));
  ui.dlPreview.addEventListener('click', () => void downloadPreview());
  ui.redecode.addEventListener('click', () => {
    if (raw && currentFile) void decodeAndShow(token);
  });
  ui.engine.value = engine;
  ui.engine.addEventListener('change', () => void switchEngine(asEngine(ui.engine.value)));
  ui.backend.addEventListener('change', () => (ui.backend.dataset.touched = '1'));
  ui.copyJson.addEventListener('click', async () => {
    if (!meta) return;
    try {
      await navigator.clipboard.writeText(JSON.stringify(meta, null, 2));
      ui.copyJson.textContent = 'Copied';
      setTimeout(() => (ui.copyJson.textContent = 'Copy as JSON'), 1400);
    } catch {
      /* ignore */
    }
  });
}

/** The GPU backend is offered with the WebGPU build only, and is its default. */
function syncBackend(build: BuildInfo) {
  const gpuOpt = ui.backend.querySelector<HTMLOptionElement>('option[value="gpu"]')!;
  const gpu = build.binary === 'gpu';
  gpuOpt.disabled = !gpu;
  gpuOpt.textContent = gpu ? 'GPU (WebGPU)' : 'GPU (WebGPU build only)';
  if (gpu && ui.backend.dataset.touched !== '1') ui.backend.value = 'gpu';
  if (!gpu && ui.backend.value === 'gpu') ui.backend.value = 'auto';
}

async function switchEngine(next: Engine) {
  engine = next;
  const p = new URLSearchParams(location.search);
  if (next === 'auto') p.delete('engine');
  else p.set('engine', next);
  const url = `${location.pathname}${p.size ? `?${p}` : ''}${location.hash}`;
  history.replaceState(null, '', url);
  ui.engine.disabled = true;
  delete ui.backend.dataset.touched;
  setStatus('Switching the WASM build…');
  let build: BuildInfo;
  try {
    build = await startEngine(next);
  } catch (e) {
    ui.engine.disabled = false;
    if (e instanceof NotAvailableError) {
      // the SIMD and scalar builds share one glue module: only a reload can swap them
      setStatus(`${e.message}. Reloading…`);
      location.reload();
      return;
    }
    setStatus(`Could not switch the WASM build: ${friendly(e)}`, true);
    return;
  }
  ui.engine.disabled = false;
  syncBackend(build);
  if (currentFile) await openFile(currentFile);
  else setStatus(`Using the ${build.binary} build. Open a file to decode it.`);
}

function setCompare(on: boolean) {
  compare = on;
  ui.compare.setAttribute('aria-pressed', String(on));
  requestRender();
}

function currentUniforms(): Uniforms {
  return uniforms(compare ? DEFAULTS : adjust, baseline);
}

let raf = 0;
function requestRender() {
  if (!raf) raf = requestAnimationFrame(renderNow);
}
let settleRaf = 0;
function drawOnce() {
  if (!renderer || !viewport) return;
  const v = viewport.view;
  renderer.draw(v, currentUniforms(), source, v.scale >= 2);
  ui.zoomLevel.textContent = `${Math.round(v.scale * 100)}%`;
}
function renderNow() {
  raf = 0;
  drawOnce();
  // WebKit can present the previous WebGL frame until the next one: draw the final state once
  // more on the following frame (cheap: one draw call).
  cancelAnimationFrame(settleRaf);
  settleRaf = requestAnimationFrame(() => {
    settleRaf = 0;
    drawOnce();
  });
}

let histTimer = 0;
let histPending = false;
function scheduleHistogram() {
  if (histTimer) {
    histPending = true;
    return;
  }
  computeHistogram();
  histTimer = window.setTimeout(() => {
    histTimer = 0;
    if (histPending) {
      histPending = false;
      scheduleHistogram();
    }
  }, 80);
}

function computeHistogram() {
  if (!renderer || !hasRaw) return drawHistogram(emptyHistogram());
  drawHistogram(renderer.histogram(currentUniforms()));
}

function drawHistogram(hg: Histogram) {
  const c = ui.hist;
  const dpr = window.devicePixelRatio || 1;
  const w = Math.round(c.clientWidth * dpr) || 600;
  const hh = Math.round(c.clientHeight * dpr) || 220;
  if (c.width !== w || c.height !== hh) {
    c.width = w;
    c.height = hh;
  }
  const ctx = c.getContext('2d')!;
  ctx.clearRect(0, 0, w, hh);
  // grid
  ctx.strokeStyle = 'rgba(14, 14, 14, 0.08)';
  ctx.lineWidth = 1;
  for (let i = 1; i < 4; i++) {
    const x = Math.round((w * i) / 4) + 0.5;
    ctx.beginPath();
    ctx.moveTo(x, 0);
    ctx.lineTo(x, hh);
    ctx.stroke();
  }
  if (!hg.total) return;
  ui.hist.dataset.mean = meanOf(hg.l).toFixed(2);
  let max = 1;
  for (const ch of [hg.r, hg.g, hg.b]) for (let i = 1; i < 255; i++) max = Math.max(max, ch[i]);
  const path = (bins: Uint32Array) => {
    ctx.beginPath();
    ctx.moveTo(0, hh);
    for (let i = 0; i < 256; i++) {
      const x = (i / 255) * w;
      const y = hh - Math.min(1, Math.sqrt(bins[i] / max)) * (hh - 4);
      ctx.lineTo(x, y);
    }
    ctx.lineTo(w, hh);
    ctx.closePath();
  };
  ctx.globalCompositeOperation = 'multiply';
  const fills: [Uint32Array, string][] = [
    [hg.r, 'rgba(217, 71, 59, 0.5)'],
    [hg.g, 'rgba(46, 154, 92, 0.5)'],
    [hg.b, 'rgba(47, 99, 214, 0.5)'],
  ];
  for (const [bins, color] of fills) {
    path(bins);
    ctx.fillStyle = color;
    ctx.fill();
  }
  ctx.globalCompositeOperation = 'source-over';
  path(hg.l);
  ctx.strokeStyle = 'rgba(14, 14, 14, 0.9)';
  ctx.lineWidth = 1.5 * dpr;
  ctx.stroke();
  const pct = (n: number) => (n / hg.total) * 100;
  const hi = Math.max(pct(hg.r[255]), pct(hg.g[255]), pct(hg.b[255]));
  const lo = pct(hg.l[0]);
  ui.clipInfo.textContent = `Clipped: highlights ${hi.toFixed(hi < 1 ? 2 : 1)} % · shadows ${lo.toFixed(lo < 1 ? 2 : 1)} %`;
}

// ------------------------------------------------------------------- auto exposure

let previewMean: number | null = null;

function meanOf(bins: Uint32Array): number {
  let n = 0;
  let sum = 0;
  for (let i = 0; i < 256; i++) {
    n += bins[i];
    sum += i * bins[i];
  }
  return n ? sum / n : 0;
}

/** Mean sRGB-encoded luma (0..255) of the embedded preview, on a 256 px downscale. */
function bitmapMeanLuma(bmp: ImageBitmap): number {
  const k = Math.min(1, 256 / Math.max(bmp.width, bmp.height));
  const w = Math.max(1, Math.round(bmp.width * k));
  const hh = Math.max(1, Math.round(bmp.height * k));
  const c = document.createElement('canvas');
  c.width = w;
  c.height = hh;
  const ctx = c.getContext('2d', { willReadFrequently: true })!;
  ctx.drawImage(bmp, 0, 0, w, hh);
  const d = ctx.getImageData(0, 0, w, hh).data;
  let sum = 0;
  for (let i = 0; i < d.length; i += 4) sum += 0.2126 * d[i] + 0.7152 * d[i + 1] + 0.0722 * d[i + 2];
  return sum / (d.length / 4);
}

/** Auto: set Exposure so the rendering's mean brightness matches the camera's preview. */
function autoExposure() {
  if (!renderer || !hasRaw || previewMean === null) return;
  const target = previewMean;
  const meanAt = (ev: number) => meanOf(renderer!.histogram(uniforms({ ...adjust, exposure: ev }, baseline)).l);
  let lo = -5;
  let hi = 5;
  for (let i = 0; i < 14; i++) {
    const mid = (lo + hi) / 2;
    if (meanAt(mid) < target) lo = mid;
    else hi = mid;
  }
  adjust.exposure = Math.round(((lo + hi) / 2) * 20) / 20;
  syncSliders();
  adjustmentsChanged();
}

// ------------------------------------------------------------------- sliders

const sliderEls = new Map<string, { input: HTMLInputElement; out: HTMLOutputElement }>();
function buildSliders() {
  let group = '';
  for (const s of SLIDERS) {
    if (s.group !== group) {
      group = s.group;
      ui.sliders.append(h('div', { class: 'slider-group', text: group }));
    }
    const id = `adj-${s.key}`;
    const input = h('input', { type: 'range', id, min: String(s.min), max: String(s.max), step: String(s.step), value: String(DEFAULTS[s.key]) }) as HTMLInputElement;
    if (s.track) input.classList.add(s.track);
    const out = h('output', { for: id, text: s.format(DEFAULTS[s.key]) }) as HTMLOutputElement;
    const row = h('div', { class: 'slider' }, h('label', { for: id, text: s.label }), out, input);
    input.setAttribute('aria-valuetext', s.format(DEFAULTS[s.key]));
    input.addEventListener('input', () => {
      adjust[s.key] = Number(input.value);
      updateSlider(s.key);
      adjustmentsChanged();
    });
    input.addEventListener('dblclick', () => {
      adjust[s.key] = DEFAULTS[s.key];
      input.value = String(DEFAULTS[s.key]);
      updateSlider(s.key);
      adjustmentsChanged();
    });
    input.title = 'Double-click to reset';
    sliderEls.set(s.key, { input, out });
    ui.sliders.append(row);
  }
}

function updateSlider(key: string) {
  const s = SLIDERS.find((x) => x.key === key)!;
  const el = sliderEls.get(key)!;
  const text = s.format(adjust[s.key]);
  el.out.textContent = text;
  el.input.setAttribute('aria-valuetext', text);
}

function syncSliders() {
  for (const s of SLIDERS) {
    sliderEls.get(s.key)!.input.value = String(adjust[s.key]);
    updateSlider(s.key);
  }
  ui.tone.value = String(adjust.tone);
}

function adjustmentsChanged() {
  if (compare) setCompare(false);
  if (source !== 'raw' && hasRaw) setSource('raw');
  requestRender();
  scheduleHistogram();
  ui.compare.disabled = isDefault(adjust);
}

// ------------------------------------------------------------------- sources

function setSource(s: Source) {
  if (s === 'preview' && !timings?.preview?.bitmap) return;
  if (s === 'raw' && !hasRaw) return;
  source = s;
  ui.srcRaw.setAttribute('aria-checked', String(s === 'raw'));
  ui.srcPreview.setAttribute('aria-checked', String(s === 'preview'));
  const size = s === 'raw' ? rawSize : previewSize();
  viewport!.setImage(size[0], size[1], s === 'raw');
  updateDims();
  requestRender();
}

function previewSize(): [number, number] {
  const p = timings?.preview;
  if (!p?.bitmap) return [0, 0];
  return orientedSize(p.bitmap.width, p.bitmap.height, p.orientation);
}

function updateDims() {
  if (source === 'preview') {
    const [w, hgt] = previewSize();
    ui.dims.textContent = `Embedded ${timings?.preview?.desc.kind ?? 'preview'} · ${w} × ${hgt}`;
  } else if (lastFrameInfo) {
    const [w, hgt] = rawSize;
    ui.dims.textContent = `RAW decode · ${w} × ${hgt} · ${lastFrameInfo.demosaic}`;
  }
}

// ------------------------------------------------------------------- open + decode

function friendly(e: unknown): string {
  if (e instanceof FlashRawError) {
    switch (e.kind) {
      case 'UnsupportedFormat':
        return 'This does not look like a RAW file FlashRaw recognises.';
      case 'UnsupportedCompression':
        return `The sensor data uses a codec FlashRaw cannot decode (${e.message}).`;
      case 'OutOfMemory':
      case 'LimitExceeded':
        return `The image is too large for this browser tab (${e.message}).`;
      case 'CorruptFile':
        return `The file is damaged: ${e.message}`;
      default:
        return `${e.kind}: ${e.message}`;
    }
  }
  return (e as Error)?.message ?? String(e);
}

export async function openFile(file: File) {
  const my = ++token;
  setupViewer();
  ui.workspace.hidden = false;
  ui.dropzone.classList.add('compact');
  const t0 = now();
  setStatus(`Opening ${file.name} (${bytes(file.size)})…`);
  overlay('Loading the decoder…');
  if (!hasRaw && !timings?.preview) ui.workspace.scrollIntoView({ behavior: 'smooth', block: 'start' });

  let build: BuildInfo;
  try {
    build = await warmUp();
    caps ??= await getCapabilities();
    syncBackend(build);
  } catch (e) {
    overlay(null);
    setStatus(`Could not load the decoder: ${friendly(e)}`, true);
    return;
  }
  if (my !== token) return;

  // reset the previous file
  raw?.close();
  raw = null;
  hasRaw = false;
  meta = null;
  lastFrameInfo = null;
  currentFile = file;
  timings?.preview?.bitmap?.close();
  timings = null;
  baseline = 0;
  previewMean = null;
  ui.autoAdjust.disabled = true;
  delete ui.hist.dataset.previewMean;
  renderer!.setPreview(null);
  for (const b of [ui.dlPng, ui.dlJpeg, ui.dlPreview, ui.redecode]) b.disabled = true;
  ui.srcPreview.disabled = true;
  ui.dlInfo.textContent = '';
  ui.fileLabel.textContent = file.name;
  ui.dims.textContent = '';
  overlay('Reading metadata…');

  try {
    const to = now();
    const r = await FlashRaw.open(file);
    const openMs = now() - to;
    if (my !== token) return r.close();
    raw = r;
    const tm = now();
    meta = r.metadata;
    const info = r.frameInfo;
    const metaMs = now() - tm;
    timings = { openMs, parseMs: r.parseMs, metaMs, preview: null };
    baseline = meta.dng?.baselineExposure != null ? meta.dng.baselineExposure + (meta.dng.profile?.baselineExposureOffset ?? 0) : LOOK.cameraEV;
    renderMetadata(ui.metaGrid, meta, info, file);
    ui.metaJson.textContent = JSON.stringify(meta, null, 2);
    renderPreviewsTable(r.previews);
    ui.stage.setAttribute('aria-label', `${meta.camera.name} RAW image, ${meta.width} × ${meta.height}`);
    setStatus(`${meta.camera.name} · ${meta.format} · metadata in ${ms(r.parseMs)} ms`);
    renderPerf(build);

    // 1. instant: the embedded preview, decoded by the browser
    if (r.previews.length) {
      overlay('Extracting the embedded preview…');
      try {
        timings.preview = await loadPreview(r);
      } catch (e) {
        console.warn('preview:', e);
      }
      if (my !== token) return;
      if (timings.preview?.bitmap) {
        renderer!.setPreview({ bitmap: timings.preview.bitmap, orientation: timings.preview.orientation });
        previewMean = bitmapMeanLuma(timings.preview.bitmap);
        ui.hist.dataset.previewMean = previewMean.toFixed(2);
        ui.srcPreview.disabled = false;
        ui.dlPreview.disabled = false;
        if (!hasRaw) setSource('preview');
      } else if (timings.preview) {
        ui.dlPreview.disabled = false;
      }
      renderPerf(build);
    }

    // 2. the RAW decode
    await decodeAndShow(my, t0);
  } catch (e) {
    if (my !== token) return;
    overlay(null);
    setStatus(friendly(e), true);
    console.error(e);
  }
}

async function loadPreview(r: FlashRaw): Promise<PreviewState | null> {
  const te = now();
  const p = await r.preview();
  const extractMs = now() - te;
  if (!p) return null;
  const blob = new Blob([p.data], { type: p.mimeType });
  const td = now();
  let bitmap: ImageBitmap | null = null;
  if (p.encoding === 'jpeg' || p.mimeType.startsWith('image/')) {
    try {
      bitmap = await createImageBitmap(blob, { imageOrientation: 'none' } as ImageBitmapOptions);
    } catch {
      bitmap = null;
    }
  }
  if (!bitmap) {
    // RGB / YCbCr previews: FlashRaw decodes them to RGBA
    try {
      const rgba = await r.preview({ index: p.index, format: 'rgba8' });
      if (rgba) bitmap = await createImageBitmap(new ImageData(rgba.data, rgba.width, rgba.height));
    } catch {
      bitmap = null;
    }
  }
  // previews larger than the GPU's texture limit: let the browser downscale them
  const max = renderer?.maxSize ?? 16384;
  if (bitmap && Math.max(bitmap.width, bitmap.height) > max) {
    const k = max / Math.max(bitmap.width, bitmap.height);
    const small = await createImageBitmap(bitmap, { resizeWidth: Math.floor(bitmap.width * k), resizeHeight: Math.floor(bitmap.height * k), resizeQuality: 'high' });
    bitmap.close();
    bitmap = small;
  }
  // Engines without `imageOrientation: 'none'` apply the JPEG's own EXIF orientation: then the
  // bitmap comes back with swapped sides and is already upright.
  let orientation = p.orientation;
  if (bitmap && orientation >= 5 && p.width !== p.height && bitmap.width > bitmap.height !== p.width > p.height) orientation = 1;
  return { desc: p, blob, bitmap, orientation, extractMs, decodeMs: now() - td };
}

async function decodeAndShow(my: number, t0 = now()) {
  if (!raw || !meta || !renderer || busy) return;
  busy = true;
  ui.redecode.disabled = true;
  const build = await warmUp();
  try {
    let demosaic = ui.demosaic.value as 'bilinear' | 'half-size';
    let note = '';
    if (demosaic === 'bilinear' && Math.max(meta.width, meta.height) > renderer.maxSize) {
      demosaic = 'half-size';
      note = ` (half-size: ${meta.width} × ${meta.height} exceeds this GPU's ${renderer.maxSize} px texture limit)`;
    }
    const backend = ui.backend.value as 'auto' | 'cpu' | 'cpu-simd' | 'gpu';
    overlay(`Decoding ${((meta.width * meta.height) / 1e6).toFixed(1)} MP…`);
    setStatus(`Decoding ${meta.camera.name} with FlashRaw…`);
    // let the overlay paint before a (possibly main-thread) decode
    await new Promise((r) => requestAnimationFrame(() => setTimeout(r, 0)));
    if (my !== token) return;
    const zeroCopy = renderer instanceof GlRenderer;
    const run = (d: 'bilinear' | 'half-size') => raw!.decode({ output: 'linear-rgb-f16', demosaic: d, backend, zeroCopy });
    let frame: DecodedFrame<'linear-rgb-f16'>;
    const td = now();
    try {
      frame = await run(demosaic);
    } catch (e) {
      if (e instanceof FlashRawError && (e.kind === 'OutOfMemory' || e.kind === 'LimitExceeded') && demosaic === 'bilinear') {
        demosaic = 'half-size';
        note = ' (half-size: the full-size decode did not fit in memory)';
        frame = await run(demosaic);
      } else throw e;
    }
    const wallMs = now() - td;
    if (my !== token) {
      frame.free();
      return;
    }
    const tu = now();
    renderer.setImage({ data: frame.data, width: frame.width, height: frame.height, orientation: frame.orientation });
    const uploadMs = now() - tu;
    const fi = {
      width: frame.width,
      height: frame.height,
      demosaic: demosaic === 'half-size' ? 'half-size demosaic' : 'bilinear demosaic',
      backend: frame.backend,
      warnings: frame.warnings.length,
      incomplete: frame.incomplete,
    };
    const ft = frame.timings;
    frame.free();
    lastFrameInfo = fi;
    hasRaw = true;
    rawSize = orientedSize(fi.width, fi.height, frame.orientation);
    Object.assign(timings!, {
      decodeMs: ft.totalMs,
      wallMs,
      uploadMs,
      stages: ft.stages as Record<string, number>,
      copyMs: ft.copyMs,
    });
    setSource('raw');
    renderNow();
    timings!.firstPixelsMs = now() - t0;
    scheduleHistogram();
    overlay(null);
    for (const b of [ui.dlPng, ui.dlJpeg, ui.redecode]) b.disabled = false;
    ui.autoAdjust.disabled = previewMean === null;
    ui.compare.disabled = isDefault(adjust);
    const mp = (fi.width * fi.height) / 1e6;
    setStatus(
      `${meta.camera.name} · decoded ${mp.toFixed(1)} MP in ${ms(ft.totalMs)} ms on the ${fi.backend.kind === 'gpu' ? 'GPU' : 'CPU'} (${BINARY_NAMES[fi.backend.binary] ?? fi.backend.binary})${note}` +
        (fi.incomplete ? ` · ${fi.warnings} warning(s): the file is truncated, missing samples are black` : ''),
    );
    renderPerf(build);
  } catch (e) {
    if (my !== token) return;
    overlay(null);
    const msg = friendly(e);
    setStatus(timings?.preview?.bitmap ? `${msg} Showing the embedded preview instead.` : msg, true);
    renderPerf(build);
    console.error(e);
  } finally {
    busy = false;
    if (raw) ui.redecode.disabled = false;
  }
}

// ------------------------------------------------------------------- previews table

function renderPreviewsTable(previews: PreviewDescriptor[]) {
  ui.previewsCard.textContent = '';
  if (!previews.length) return;
  const tbody = h('tbody');
  for (const p of previews) {
    const btn = h('button', { type: 'button', class: 'link-btn', text: 'Download' }) as HTMLButtonElement;
    btn.setAttribute('aria-label', `Download ${p.kind} ${p.width} × ${p.height}`);
    btn.addEventListener('click', async () => {
      if (!raw) return;
      const x = await raw.preview({ index: p.index });
      if (x) saveBlob(new Blob([x.data], { type: x.mimeType }), `${baseName()}-${p.kind}-${p.index}.${extOf(x.mimeType)}`);
    });
    tbody.append(
      h('tr', {},
        h('td', { text: p.kind }),
        h('td', { text: p.encoding }),
        h('td', { class: 'num', text: `${p.width} × ${p.height}` }),
        h('td', { class: 'num', text: bytes(p.byteLength) }),
        h('td', { class: 'num', text: String(p.orientation) }),
        h('td', {}, btn),
      ),
    );
  }
  const table = h('table', { class: 'plain' },
    h('thead', {}, h('tr', {}, ...['Kind', 'Encoding', 'Size', 'Bytes', 'Orientation'].map((t, i) => h('th', { scope: 'col', text: t, class: i >= 2 ? 'num' : '' })),
        h('th', { scope: 'col' }, h('span', { class: 'visually-hidden', text: 'Download' })))),
    tbody,
  );
  ui.previewsCard.append(h('div', { class: 'previews-card' }, h('h4', { text: `Embedded previews (${previews.length})` }), h('div', { class: 'table-scroll' }, table)));
}

// ------------------------------------------------------------------- performance panel

const STAGES: [string, string, string][] = [
  ['containerParse', 'Container parse', '#c9c6be'],
  ['metadata', 'Metadata', '#a3a3a3'],
  ['entropyDecode', 'Entropy decode', '#0e0e0e'],
  ['unpack', 'Unpack', '#3a3a3a'],
  ['predictor', 'Predictor', '#5c5c5c'],
  ['calibration', 'Calibration', '#7a7a7a'],
  ['postprocess', 'Develop (demosaic, WB, colour)', '#262626'],
  ['gpuUpload', 'GPU upload', '#4a4a4a'],
  ['gpuCompute', 'GPU compute', '#1a1a1a'],
  ['gpuDownload', 'GPU download', '#6b6b6b'],
  ['copy', 'Copy out of WASM', '#dcd9d2'],
  ['texture', 'Texture upload (WebGL)', '#b5b2aa'],
];

function tile(label: string, value: string, unit: string, sub: string, hero = false) {
  return h('div', { class: `tile${hero ? ' hero-tile' : ''}` },
    h('div', { class: 'tile-label', text: label }),
    h('div', { class: 'tile-value' }, value, unit ? h('small', { text: unit }) : null),
    h('div', { class: 'tile-sub', text: sub }),
  );
}

const BINARY_NAMES: Record<string, string> = {
  gpu: 'WebGPU build',
  threads: 'Threaded build',
  simd: 'Single-threaded SIMD build',
  fallback: 'Scalar build',
  'simd-jxl': 'SIMD build with JPEG XL',
  'threads-jxl': 'Threaded build with JPEG XL',
};

/** "WebGPU build · GPU backend (adapter)" or "Threaded build · CPU, 16 threads". */
function ranOn(build: BuildInfo, be: DecodedFrame['backend'] | undefined) {
  const bin = BINARY_NAMES[be?.binary ?? build.binary] ?? build.binary;
  if (!be) return `${bin} · not decoded yet`;
  return be.kind === 'gpu'
    ? `${bin} · GPU backend${be.gpuAdapter ? ` (${be.gpuAdapter})` : ''}`
    : `${bin} · CPU backend, ${be.threads} thread${be.threads === 1 ? '' : 's'}`;
}

function renderEngine(build: BuildInfo) {
  const be = lastFrameInfo?.backend;
  const why = be?.fallbackReason
    ? `GPU requested, ran on the CPU: ${be.fallbackReason}`
    : be && be.kind === 'cpu' && /gpu/i.test(be.requested)
      ? `GPU requested; FlashRaw developed this file on the CPU${meta?.format === 'DNG' ? ' (its DNG opcode lists are applied by the CPU pipeline)' : ''}`
      : engine === 'auto'
        ? engineNote
        : 'selected in Decoder options';
  ui.perfEngine.replaceChildren(h('span', { class: 'engine-label', text: 'Ran on' }), h('b', { text: ranOn(build, be) }), ...(why ? [h('span', { class: 'engine-why', text: why })] : []));
  ui.perfEngine.dataset.kind = be?.kind ?? '';
  ui.perfEngine.dataset.binary = be?.binary ?? build.binary;
}

function renderPerf(build: BuildInfo) {
  const t = timings;
  if (!t || !meta) return;
  renderEngine(build);
  const p = t.preview;
  const mp = (meta.width * meta.height) / 1e6; // sensor image pixels (also for half-size output)
  ui.perfTiles.replaceChildren(
    tile('Metadata', ms(t.parseMs), 'ms', `open() ${ms(t.openMs)} ms incl. reading ${bytes(currentFile?.size ?? 0)} into WASM`),
    tile('Embedded preview', p ? ms(p.extractMs + p.decodeMs) : '–', p ? 'ms' : '', p ? `${p.desc.width} × ${p.desc.height} ${p.desc.encoding}: extract ${ms(p.extractMs)} ms, browser decode ${ms(p.decodeMs)} ms` : meta.previews.length ? 'not decodable here' : 'none in this file'),
    tile('RAW decode', ms(t.decodeMs), t.decodeMs != null ? 'ms' : '', t.wallMs != null ? `to linear RGB, decode() wall ${ms(t.wallMs)} ms` : 'pending', true),
    tile('Throughput', t.decodeMs ? (mp / (t.decodeMs / 1000)).toFixed(0) : '–', t.decodeMs ? 'MP/s' : '', `${mp.toFixed(1)} MP decoded`),
    tile('File to pixels', t.firstPixelsMs != null ? ms(t.firstPixelsMs) : '–', t.firstPixelsMs != null ? 'ms' : '', t.uploadMs != null ? `incl. WASM init (first file) and texture upload ${ms(t.uploadMs)} ms` : 'open + decode + upload + first draw'),
  );

  ui.perfStages.textContent = '';
  if (t.stages) {
    const values = { ...t.stages, copy: t.copyMs ?? 0, texture: t.uploadMs ?? 0 } as Record<string, number>;
    const items = STAGES.filter(([k]) => (values[k] ?? 0) > 0.005);
    const total = items.reduce((a, [k]) => a + values[k], 0);
    const bar = h('div', { class: 'stage-bar', role: 'img', 'aria-label': items.map(([k, label]) => `${label} ${ms(values[k])} ms`).join(', ') });
    const legend = h('ul', { class: 'stage-legend' });
    for (const [k, label, color] of items) {
      const seg = h('span', { title: `${label}: ${ms(values[k])} ms` });
      seg.style.flex = `${values[k] / total} 1 0`;
      seg.style.background = color;
      bar.append(seg);
      const key = h('i', { class: 'sw' });
      key.style.background = color;
      legend.append(h('li', {}, key, `${label} `, h('b', { text: `${ms(values[k])} ms` })));
    }
    ui.perfStages.append(h('p', { class: 'stages-title', text: 'Where the time goes (FlashRaw stages, then the page)' }), bar, legend);
  }

  const be = lastFrameInfo?.backend;
  const threads = caps?.threads;
  const rows: [string, string][] = [
    ['Decoder', be ? be.decode : '–'],
    ['Develop', be?.postprocess ?? '–'],
    ['WASM binary', `${be?.binary ?? build.binary}${(be?.jpegxl ?? false) ? ' (JPEG XL variant)' : ''} · ${build.simd ? build.simdLevel || 'SIMD128' : 'scalar'}`],
    ['Threads', build.threads ? `${build.threadCount} (shared-memory WASM threads)` : `1 · ${build.threadsReason ?? threads?.reason ?? 'single-threaded build'}`],
    ['Cross-origin isolated', threads?.crossOriginIsolated ? 'yes' : 'no'],
    ['GPU (WebGPU)', build.gpu ? (be?.gpuAdapter ?? (caps?.gpu.available ? 'available' : caps?.gpu.reason ?? 'unavailable')) : 'not in this build'],
    ['Requested backend', be ? `${be.requested} → ${be.kind}${be.fallbackReason ? ` (fallback: ${be.fallbackReason})` : ''}` : '–'],
    ['WASM init', `${ms(build.initMs)} ms (download + compile ${ms(initWall)} ms)`],
    ['Output', lastFrameInfo ? `linear-rgb-f16 ${lastFrameInfo.width} × ${lastFrameInfo.height}, ${lastFrameInfo.demosaic}` : '–'],
    ['Renderer', `${renderer?.kind ?? '–'}, adjustments live on linear light`],
    ['FlashRaw', `v${build.version}`],
    ['JPEG XL support', build.jpegxl ? 'loaded' : build.jpegxlMode === 'never' ? 'off' : 'on demand'],
  ];
  ui.perfBackend.replaceChildren(...rows.flatMap(([k, v]) => [h('dt', { text: k }), h('dd', { text: v })]));
}

// ------------------------------------------------------------------- downloads

function baseName() {
  return (currentFile?.name ?? 'image').replace(/\.[^.]+$/, '');
}
const extOf = (mime: string) => (mime === 'image/jpeg' ? 'jpg' : mime.split('/')[1]?.replace('x-', '') || 'bin');

function saveBlob(blob: Blob, name: string) {
  const url = URL.createObjectURL(blob);
  const a = h('a', { href: url, download: name });
  document.body.append(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 30_000);
}

async function download(type: 'image/png' | 'image/jpeg') {
  if (!renderer || !hasRaw) return;
  const btns = [ui.dlPng, ui.dlJpeg];
  btns.forEach((b) => (b.disabled = true));
  const t = now();
  try {
    ui.dlInfo.textContent = 'Rendering at full resolution…';
    const canvas = await renderer.exportCanvas(uniforms(adjust, baseline), (f) => (ui.dlInfo.textContent = `Rendering at full resolution… ${Math.round(f * 100)} %`));
    ui.dlInfo.textContent = `Encoding ${type === 'image/png' ? 'PNG' : 'JPEG'}…`;
    const blob = await new Promise<Blob | null>((r) => canvas.toBlob(r, type, 0.92));
    const { width, height } = canvas;
    canvas.width = canvas.height = 0;
    if (!blob) throw new Error('the browser could not encode the image');
    saveBlob(blob, `${baseName()}.${type === 'image/png' ? 'png' : 'jpg'}`);
    ui.dlInfo.textContent = `${width} × ${height} ${type === 'image/png' ? 'PNG' : 'JPEG'}, ${bytes(blob.size)} in ${((now() - t) / 1000).toFixed(1)} s`;
  } catch (e) {
    ui.dlInfo.textContent = `Export failed: ${(e as Error).message}`;
  } finally {
    btns.forEach((b) => (b.disabled = false));
  }
}

async function downloadPreview() {
  const p = timings?.preview;
  if (!p) return;
  saveBlob(p.blob, `${baseName()}-preview.${extOf(p.blob.type)}`);
}
