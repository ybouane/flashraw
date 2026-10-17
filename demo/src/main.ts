// Entry point: kept tiny. The decoder (WASM) and the viewer are preloaded once the page has
// loaded and the main thread is idle (or earlier, when the visitor shows intent: hovers/drags
// over the drop zone, focuses the file picker, picks a file); the sample file is prefetched then
// too, except on slow or data-saving connections. The benchmark charts load when their section
// scrolls near.

type AppModule = typeof import('./app');

let appPromise: Promise<AppModule> | null = null;
function loadApp(): Promise<AppModule> {
  appPromise ??= import('./app').then((m) => {
    m.warmUp();
    return m;
  });
  return appPromise;
}

const dropzone = document.getElementById('dropzone')!;
const input = document.getElementById('file-input') as HTMLInputElement;
const statusEl = document.getElementById('status')!;

async function openFile(file: File | undefined) {
  if (!file) return;
  statusEl.classList.remove('error');
  statusEl.textContent = `Loading the decoder for ${file.name}…`;
  try {
    const app = await loadApp();
    await app.openFile(file);
  } catch (e) {
    statusEl.classList.add('error');
    statusEl.textContent = `Could not load the decoder: ${(e as Error).message}`;
    console.error(e);
  }
}

input.addEventListener('change', () => {
  void openFile(input.files?.[0]);
  input.value = '';
});

// "Try a sample": a small CC0 file from raw.pixls.us, prefetched when idle (see below).
const sampleBtn = document.getElementById('sample-btn') as HTMLButtonElement | null;
let samplePromise: Promise<Blob> | null = null;
function fetchSample(low = false): Promise<Blob> {
  samplePromise ??= fetch(sampleBtn!.dataset.src!, low ? { priority: 'low' } : {}).then((res) => {
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    return res.blob();
  });
  samplePromise.catch(() => (samplePromise = null));
  return samplePromise;
}
sampleBtn?.addEventListener('click', async () => {
  const name = sampleBtn.dataset.name ?? sampleBtn.dataset.src!.split('/').pop()!;
  sampleBtn.disabled = true;
  statusEl.classList.remove('error');
  statusEl.textContent = `Downloading the sample (${name})…`;
  void loadApp();
  try {
    const blob = await fetchSample();
    await openFile(new File([blob], name, { type: 'application/octet-stream' }));
  } catch (e) {
    statusEl.classList.add('error');
    statusEl.textContent = `Could not download the sample: ${(e as Error).message}`;
  } finally {
    sampleBtn.disabled = false;
  }
});

// Prefetch on intent.
for (const ev of ['pointerenter', 'focusin', 'touchstart'] as const) {
  dropzone.addEventListener(ev, () => void loadApp(), { once: true, passive: true });
}

// Preload when idle, after `load`: the decoder chunk, the WASM (compiled while it streams) and
// its JS glue, so the first decode starts at once; and the sample file, unless the connection is
// slow or the visitor asked to save data. Low priority, never before first paint.
interface NetworkInfo { saveData?: boolean; effectiveType?: string }
function slowConnection() {
  const c = (navigator as Navigator & { connection?: NetworkInfo }).connection;
  return !!c && (c.saveData === true || ['slow-2g', '2g', '3g'].includes(c.effectiveType ?? ''));
}
const idle = (fn: () => void) =>
  'requestIdleCallback' in window ? requestIdleCallback(fn, { timeout: 4000 }) : setTimeout(fn, 1500);
function preload() {
  // two idle slots: the decoder first, then the sample, so neither competes with the other
  idle(() => {
    void loadApp().catch(() => {});
    if (sampleBtn && !slowConnection()) idle(() => void fetchSample(true).catch(() => {}));
  });
}
if (document.readyState === 'complete') preload();
else window.addEventListener('load', preload, { once: true });

// Drag and drop anywhere on the page (the drop zone highlights).
let dragDepth = 0;
const hasFiles = (e: DragEvent) => Array.from(e.dataTransfer?.types ?? []).includes('Files');
window.addEventListener('dragenter', (e) => {
  if (!hasFiles(e)) return;
  e.preventDefault();
  dragDepth++;
  dropzone.dataset.state = 'drag';
  void loadApp();
});
window.addEventListener('dragover', (e) => {
  if (!hasFiles(e)) return;
  e.preventDefault();
  e.dataTransfer!.dropEffect = 'copy';
});
window.addEventListener('dragleave', () => {
  dragDepth = Math.max(0, dragDepth - 1);
  if (!dragDepth) dropzone.dataset.state = 'idle';
});
window.addEventListener('drop', (e) => {
  if (!hasFiles(e)) return;
  e.preventDefault();
  dragDepth = 0;
  dropzone.dataset.state = 'idle';
  const file = e.dataTransfer?.files?.[0];
  if (file) {
    if (!dropzone.classList.contains('compact')) dropzone.scrollIntoView({ block: 'start' });
    void openFile(file);
  }
});

// ------------------------------------------------------------------ copy buttons
async function copyText(text: string, el: HTMLElement) {
  try {
    await navigator.clipboard.writeText(text);
    el.classList.add('copied');
    const target = el.classList.contains('copy-btn') ? el : el.querySelector<HTMLElement>('.copy-label');
    const label = target?.textContent ?? null;
    if (target) target.textContent = 'Copied';
    setTimeout(() => {
      el.classList.remove('copied');
      if (target && label) target.textContent = label;
    }, 1400);
  } catch {
    /* clipboard unavailable: ignore */
  }
}
document.addEventListener('click', (e) => {
  const el = (e.target as HTMLElement).closest<HTMLElement>('[data-copy], [data-copy-from]');
  if (!el) return;
  const text = el.dataset.copy ?? el.previousElementSibling?.textContent ?? '';
  void copyText(text, el);
});

// ------------------------------------------------------------------ tabs (WAI-ARIA)
for (const root of document.querySelectorAll<HTMLElement>('[data-tabs]')) {
  const tabs = Array.from(root.querySelectorAll<HTMLButtonElement>('[role="tab"]'));
  const select = (tab: HTMLButtonElement, focus = false) => {
    for (const t of tabs) {
      const on = t === tab;
      t.setAttribute('aria-selected', String(on));
      t.tabIndex = on ? 0 : -1;
      document.getElementById(t.getAttribute('aria-controls')!)!.hidden = !on;
    }
    if (focus) tab.focus();
  };
  for (const t of tabs) {
    t.addEventListener('click', () => select(t));
    t.addEventListener('keydown', (e) => {
      const i = tabs.indexOf(t);
      const next = e.key === 'ArrowRight' ? i + 1 : e.key === 'ArrowLeft' ? i - 1 : e.key === 'Home' ? 0 : e.key === 'End' ? tabs.length - 1 : null;
      if (next === null) return;
      e.preventDefault();
      select(tabs[(next + tabs.length) % tabs.length], true);
    });
  }
}

// ------------------------------------------------------------------ scrolling tables
// `.table-wrap > .table-scroll`: data-scrolls while the table is wider than its box, data-more
// while there is more to the right (shows the edge fade and the hint above).
for (const wrap of document.querySelectorAll<HTMLElement>('.table-wrap')) {
  const box = wrap.querySelector<HTMLElement>('.table-scroll')!;
  const update = () => {
    const scrolls = box.scrollWidth > box.clientWidth + 1;
    wrap.toggleAttribute('data-scrolls', scrolls);
    wrap.toggleAttribute('data-more', scrolls && box.scrollLeft + box.clientWidth < box.scrollWidth - 2);
  };
  box.addEventListener('scroll', update, { passive: true });
  new ResizeObserver(update).observe(box);
}

// ------------------------------------------------------------------ charts (lazy)
const charts = document.getElementById('charts')!;
const loadCharts = () => import('./charts').then((m) => m.renderCharts(charts));
if ('IntersectionObserver' in window) {
  const io = new IntersectionObserver(
    (entries) => {
      if (entries.some((e) => e.isIntersecting)) {
        io.disconnect();
        void loadCharts();
      }
    },
    { rootMargin: '600px 0px' },
  );
  io.observe(charts);
} else {
  void loadCharts();
}

export {};
