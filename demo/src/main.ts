// Entry point: kept tiny. The decoder (WASM) and the viewer are loaded on demand, when the
// visitor shows intent (hovers/drags over the drop zone, focuses the file picker) or picks a
// file; the benchmark charts load when their section scrolls near.

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

// Prefetch on intent.
for (const ev of ['pointerenter', 'focusin', 'touchstart'] as const) {
  dropzone.addEventListener(ev, () => void loadApp(), { once: true, passive: true });
}

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
    const label = el.classList.contains('copy-btn') ? el.textContent : null;
    if (label) el.textContent = 'Copied';
    setTimeout(() => {
      el.classList.remove('copied');
      if (label) el.textContent = label;
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
