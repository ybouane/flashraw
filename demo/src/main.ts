// Entry point: kept tiny. The app chunk starts downloading as soon as this module runs. Right after
// the first paint the WASM (compiled while it streams), its instantiation and the worker threads
// start, so the decoder is ready before the visitor picks a file. The sample file is downloaded in
// parallel at low priority, so "Try a sample" decodes at once (skipped on slow or metered
// connections and when the visitor asked to save data; then it is fetched when the visitor
// approaches the button). The benchmark charts load when their section scrolls near.

type AppModule = typeof import('./app');

// The chunk (JavaScript only, no WASM yet) is requested at once, not after the first paint.
const appChunk: Promise<AppModule> = import('./app');
appChunk.catch(() => {});
let warmed = false;
function loadApp(): Promise<AppModule> {
  return appChunk.then((m) => {
    if (!warmed) {
      warmed = true;
      void m.warmUp().catch(() => (warmed = false));
    }
    return m;
  });
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

// "Try a sample": a small CC0 file from raw.pixls.us, preloaded (see below).
const sampleBtn = document.getElementById('sample-btn') as HTMLButtonElement | null;
let samplePromise: Promise<Blob> | null = null;
function fetchSample(): Promise<Blob> {
  samplePromise ??= fetch(sampleBtn!.dataset.src!, { priority: 'low' } as RequestInit).then((res) => {
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    return res.blob();
  });
  samplePromise.catch(() => (samplePromise = null));
  return samplePromise;
}
// Fallback when the automatic prefetch is skipped: start the download when the visitor approaches.
for (const ev of ['pointerenter', 'focus', 'touchstart'] as const) {
  sampleBtn?.addEventListener(ev, () => void fetchSample().catch(() => {}), { once: true, passive: true });
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

// Preload as soon as the first frame is painted (not at `load`, which also waits for every
// subresource): the decoder (downloaded, compiled, instantiated, threads started) and the sample
// file start together. The sample fetch is low priority so it never delays the WASM. A sample
// download that fails is retried on click. A file picked or dropped earlier starts the decoder at
// once. The sample is skipped on 2g/3g connections and with Save-Data.
const slowOrMetered = () => {
  const c = (navigator as Navigator & { connection?: { saveData?: boolean; effectiveType?: string } }).connection;
  return c?.saveData === true || c?.effectiveType === 'slow-2g' || c?.effectiveType === '2g' || c?.effectiveType === '3g';
};
let preloaded = false;
function preload() {
  if (preloaded) return;
  preloaded = true;
  if (sampleBtn && !slowOrMetered()) void fetchSample().catch(() => {});
  void loadApp().catch(() => {});
}
function schedulePreload() {
  requestAnimationFrame(() => setTimeout(preload, 0));
  setTimeout(preload, 1500); // background tabs do not run animation frames
}
if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', schedulePreload, { once: true });
else schedulePreload();

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

// ------------------------------------------------------------------ compact menu (narrow screens)
// A <details> disclosure: closes on Escape (focus back to the summary), on a click outside and
// when a link is followed.
const navMenu = document.getElementById('nav-menu') as HTMLDetailsElement | null;
if (navMenu) {
  const summary = navMenu.querySelector('summary')!;
  navMenu.addEventListener('keydown', (e) => {
    if (e.key === 'Escape' && navMenu.open) {
      navMenu.open = false;
      summary.focus();
    }
  });
  navMenu.addEventListener('click', (e) => {
    if ((e.target as HTMLElement).closest('a')) navMenu.open = false;
  });
  document.addEventListener('click', (e) => {
    if (navMenu.open && !navMenu.contains(e.target as Node)) navMenu.open = false;
  });
}

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
