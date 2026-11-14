// Benchmark charts (plain SVG): headline speedups, per-camera and per-format wall times.
// Data: `virtual:charts-data` (docs/charts/data.json).
//
// Both bar charts show wall times in ms on one linear axis from 0: a FlashRaw bar (ink) and one
// bar per other library (greys), each labelled with its time and, for the others, how many times
// FlashRaw's time it is. A dashed line marks FlashRaw's time across each row. The axis ends near
// the 90th percentile of the values shown, so a few slow outliers do not flatten every other bar;
// longer bars are cut at the axis end with a break mark and still labelled with their value.

import data from 'virtual:charts-data';
import type { ChartsData } from './charts-schema';
import { h } from './metadata';

const D = data as ChartsData;
const SVGNS = 'http://www.w3.org/2000/svg';
// Monochrome series (the brand has no chart hues): dark and mid grey, both ≥ 3:1 on paper.
const COLORS: Record<string, string> = { flashraw: '#0e0e0e', libraw: '#5c5c5c', rawspeed: '#9a9a9a' };
const color = (l: string) => COLORS[l] ?? '#6b6b6b';
const libName = (id: string) => D.libraries.find((l) => l.id === id)?.name ?? id;
const machineName = (id: string) => {
  const m = D.machines.find((x) => x.id === id);
  return m ? (m.cpu ?? m.name) : id;
};

function s<K extends keyof SVGElementTagNameMap>(tag: K, attrs: Record<string, string | number> = {}, text?: string) {
  const el = document.createElementNS(SVGNS, tag);
  for (const [k, v] of Object.entries(attrs)) el.setAttribute(k, String(v));
  if (text !== undefined) el.textContent = text;
  return el;
}

const fmtMs = (v: number) => (v < 10 ? v.toFixed(1) : Math.round(v).toLocaleString('en-US'));
const fmtX = (v: number) => `${v < 10 ? v.toFixed(2).replace(/0$/, '') : v.toFixed(1).replace(/\.0$/, '')}×`;
/** Short bar label: 0.56×, 3.8×, 21× */
const fmtBar = (v: number) => `${v < 1 ? v.toFixed(2) : v < 10 ? v.toFixed(1) : Math.round(v)}×`;

// ----------------------------------------------------------------- tooltip

let tip: HTMLDivElement | null = null;
function showTip(target: Element, content: Node) {
  tip ??= document.body.appendChild(h('div', { class: 'tooltip', role: 'tooltip' }) as HTMLDivElement);
  tip.replaceChildren(content);
  tip.hidden = false;
  const r = target.getBoundingClientRect();
  const tw = tip.offsetWidth;
  const th = tip.offsetHeight;
  let x = r.left + r.width / 2 - tw / 2;
  x = Math.max(8, Math.min(window.innerWidth - tw - 8, x));
  let y = r.top - th - 8;
  if (y < 8) y = r.bottom + 8;
  tip.style.left = `${x}px`;
  tip.style.top = `${y}px`;
}
function hideTip() {
  if (tip) tip.hidden = true;
}
function tipRows(title: string, sub: string, rows: [string, string, string][]) {
  return h('div', {},
    h('b', { text: title }),
    h('div', { style: 'opacity:.75', text: sub }),
    ...rows.map(([lib, label, value]) => {
      const key = h('span', { class: 'tt-key' });
      key.style.background = lib === 'flashraw' ? '#fff' : color(lib);
      key.style.borderColor = lib === 'flashraw' ? '#fff' : color(lib);
      return h('div', { class: 'tt-row' }, key, `${label}: `, h('b', { text: value }));
    }),
  );
}

// ----------------------------------------------------------------- controls

function segmented(label: string, options: [string, string][], value: string, onChange: (v: string) => void) {
  const group = h('div', { class: 'seg', role: 'radiogroup', 'aria-label': label });
  const buttons = options.map(([v, text]) => {
    const b = h('button', { type: 'button', role: 'radio', 'aria-checked': String(v === value), text }) as HTMLButtonElement;
    b.tabIndex = v === value ? 0 : -1;
    b.addEventListener('click', () => select(v));
    b.addEventListener('keydown', (e) => {
      const i = options.findIndex(([x]) => x === v);
      const d = e.key === 'ArrowRight' || e.key === 'ArrowDown' ? 1 : e.key === 'ArrowLeft' || e.key === 'ArrowUp' ? -1 : 0;
      if (!d) return;
      e.preventDefault();
      const n = options[(i + d + options.length) % options.length][0];
      select(n);
      buttons[options.findIndex(([x]) => x === n)].focus();
    });
    group.append(b);
    return b;
  });
  function select(v: string) {
    options.forEach(([x], i) => {
      buttons[i].setAttribute('aria-checked', String(x === v));
      buttons[i].tabIndex = x === v ? 0 : -1;
    });
    onChange(v);
  }
  return group;
}

function legend(libs: string[]) {
  return h('ul', { class: 'legend' },
    ...libs.map((lib) => {
      const k = h('span', { class: 'key' });
      k.style.background = color(lib);
      return h('li', {}, k, libName(lib));
    }),
    h('li', {}, h('span', { class: 'key key-ref' }), "FlashRaw's time"),
    h('li', { class: 'legend-note', text: '“182 ms · 3.2×” = 3.2 times FlashRaw’s time' }),
  );
}

// ----------------------------------------------------------------- linear time bars

interface BarRow {
  label: string;
  /** second line on narrow screens / right-aligned detail */
  detail: string;
  /** wall times in ms, FlashRaw first */
  values: [lib: string, ms: number | null][];
  aria: string;
  tip: () => Node;
}

const quantile = (sorted: number[], q: number) => sorted[Math.min(sorted.length - 1, Math.max(0, Math.round(q * (sorted.length - 1))))];

/** A round step giving at most `maxTicks` intervals up to `max`. */
function niceStep(max: number, maxTicks: number) {
  for (const m of [1, 10, 100, 1000, 10000]) for (const st of [1, 2, 2.5, 5]) if (max / (st * m) <= maxTicks) return st * m;
  return 50000;
}

/** Axis end: the values' 90th percentile with headroom (or the max when that is close), rounded up. */
function axisMax(values: number[], maxTicks: number) {
  const v = [...values].sort((a, b) => a - b);
  if (!v.length) return 100;
  const top = v[v.length - 1];
  const p90 = quantile(v, 0.9);
  const target = top <= p90 * 1.35 ? top : p90 * 1.15;
  const st = niceStep(target, maxTicks);
  return Math.ceil(target / st - 1e-9) * st;
}

/** Bar label: "57 ms" for FlashRaw, "182 ms · 3.2×" for the others. */
const barLabel = (v: number, fr: number | null, isFr: boolean) => `${fmtMs(v)} ms${isFr || fr == null ? '' : ` · ${fmtBar(v / fr)}`}`;

function timeBars(root: HTMLElement, rows: BarRow[], ariaLabel: string) {
  const width = Math.max(280, root.clientWidth || 900);
  const narrow = width < 640;
  const series = rows[0]?.values.length ?? 1;
  const barH = series > 2 ? 13 : 15;
  const gap = 3;
  const barsH = series * barH + (series - 1) * gap;
  const labelW = narrow ? 0 : Math.min(250, Math.round(width * 0.27));
  const headH = 22;
  const rowPad = narrow ? 10 : 9;
  const labelLine = narrow ? 20 : 0;
  const rowH = labelLine + barsH + rowPad * 2;
  const x0 = labelW + (narrow ? 0 : 12);
  const valueW = narrow ? 92 : 112; // room for the value label past the axis end
  const x1 = width - valueW;
  const all = rows.flatMap((r) => r.values.map(([, v]) => v).filter((v): v is number => v != null));
  const ticksMax = narrow ? 3 : 7;
  const max = axisMax(all, ticksMax);
  const step = niceStep(max, ticksMax);
  const x = (v: number) => x0 + (Math.min(v, max) / max) * (x1 - x0);
  const height = headH + rows.length * rowH + 4;
  const svg = s('svg', { class: 'chart-svg', viewBox: `0 0 ${width} ${height}`, width, height, role: 'group', 'aria-label': ariaLabel });

  // grid + tick labels (top); the unit on the last tick
  for (let t = 0; t <= max + 1e-9; t += step) {
    const tx = x(t);
    const last = t + step > max + 1e-9;
    svg.append(s('line', { class: 'grid', x1: tx, x2: tx, y1: headH - 4, y2: height - 4 }));
    svg.append(s('text', { x: tx, y: 12, 'text-anchor': t === 0 ? 'start' : 'middle', class: 't-muted' }, `${t.toLocaleString('en-US')}${last ? ' ms' : ''}`));
  }

  rows.forEach((r, i) => {
    const top = headH + i * rowH;
    const g = s('g', { class: 'row', tabindex: 0, role: 'img', 'aria-label': r.aria });
    g.append(s('rect', { class: 'row-hit', x: 0, y: top, width, height: rowH }));
    if (narrow) {
      g.append(s('text', { x: 0, y: top + rowPad + 11, class: 't-strong' }, r.label.length > 34 ? r.label.slice(0, 33) + '…' : r.label));
      g.append(s('text', { x: width, y: top + rowPad + 11, 'text-anchor': 'end', class: 't-muted' }, r.detail));
    } else {
      const chars = Math.floor(labelW / 7.4);
      g.append(s('text', { x: 0, y: top + rowH / 2 + 5, class: 't-strong' }, r.label.length > chars ? r.label.slice(0, chars - 1) + '…' : r.label));
    }
    const by0 = top + rowPad + labelLine;
    const fr = r.values[0]?.[1] ?? null;
    r.values.forEach(([lib, v], k) => {
      const y = by0 + k * (barH + gap);
      const cy = y + barH / 2 + 4;
      if (v == null) {
        g.append(s('text', { x: (fr != null ? x(fr) : x0) + 6, y: cy, class: 't-muted t-small t-halo' }, `${libName(lib)}: cannot decode`));
        return;
      }
      const over = v > max;
      const w = Math.max(1.5, x(v) - x0);
      g.append(s('rect', { x: x0, y, width: w, height: barH, fill: color(lib) }));
      if (over) {
        // break mark: two paper-coloured slashes near the cut end
        for (const dx of [10, 15]) g.append(s('line', { x1: x0 + w - dx, x2: x0 + w - dx + 4, y1: y + barH + 1, y2: y - 1, stroke: '#fff', 'stroke-width': 1.5 }));
      }
      const isFr = lib === 'flashraw';
      const text = barLabel(v, fr, isFr);
      // past the bar end, or inside it (paper on the bar) when it would run off the chart
      if (x0 + w + 5 + text.length * 6.8 > width && w > text.length * 6.8 + 24)
        g.append(s('text', { x: x0 + w - (over ? 22 : 6), y: cy, 'text-anchor': 'end', class: 't-num t-in' }, text));
      else g.append(s('text', { x: x0 + w + 5, y: cy, class: `${isFr || over ? 't-num' : 't-val'} t-halo` }, text));
    });
    // FlashRaw's time across the row's bars
    if (fr != null) {
      const fx = x(fr);
      g.append(s('line', { class: 'ref', x1: fx, x2: fx, y1: by0 - 4, y2: by0 + barsH + 4 }));
    }
    g.addEventListener('pointerenter', () => showTip(g, r.tip()));
    g.addEventListener('pointerleave', hideTip);
    g.addEventListener('focus', () => showTip(g, r.tip()));
    g.addEventListener('blur', hideTip);
    svg.append(g);
  });
  return { svg, max };
}

// ----------------------------------------------------------------- headline

function headlineTiles() {
  const pick = (metric: string, vs: string, machine: string) =>
    D.headline.find((x) => x.metric === metric && x.vs === vs && x.machine === machine && x.threads === 'all' && x.set === 'popular');
  const defs: [string, string, string][] = [
    ['decode', 'libraw', `RAW decode vs ${libName('libraw')}`],
    ['decode', 'rawspeed', `RAW decode vs ${libName('rawspeed')}`],
    ['rgb', 'libraw', `RAW → display RGB vs ${libName('libraw')}`],
  ];
  return h('div', { class: 'headline-tiles' }, ...defs.map(([metric, vs, label]) =>
    h('div', { class: 'hl-tile' },
      h('div', { class: 'hl-label', text: label }),
      h('div', { class: 'hl-values' }, ...D.machines.map((m) => {
        const v = pick(metric, vs, m.id);
        return h('div', { class: 'hl-value' },
          h('span', { class: 'hl-num', text: v ? fmtX(v.median) : '–' }),
          h('span', { class: 'hl-machine', text: `${m.cpu ?? m.name}${v?.wins != null ? ` · faster on ${v.wins} of ${v.files}` : ''}` }),
        );
      })),
    ),
  ));
}

// ----------------------------------------------------------------- per camera

interface CamState { machine: string; metric: 'decode' | 'rgb'; all: boolean }

const others = (metric: 'decode' | 'rgb') => (metric === 'decode' ? ['libraw', 'rawspeed'] : ['libraw']);

function cameraChart(root: HTMLElement, st: CamState) {
  const rows = D.cameras.filter((c) => c.machine === st.machine && c[st.metric].flashraw != null);
  const libs = ['flashraw', ...others(st.metric).filter((l) => rows.some((r) => r[st.metric][l] != null))];
  const shown = st.all ? rows : rows.slice(0, 18);
  const bars: BarRow[] = shown.map((r) => {
    const fr = r[st.metric].flashraw!;
    const values = libs.map((l) => [l, r[st.metric][l] ?? null] as [string, number | null]);
    return {
      label: r.camera,
      detail: `${r.format} · ${r.mp ?? '?'} MP`,
      values,
      aria: `${r.camera}, ${r.format} ${r.codec}: ${values.map(([l, v]) => `${libName(l)} ${v == null ? 'cannot decode it' : barLabel(v, fr, l === 'flashraw')}`).join(', ')}`,
      tip: () => tipRows(r.camera, `${r.format} · ${r.codec}${r.mp ? ` · ${r.mp} MP` : ''}`, values.map(([l, v]) => [l, libName(l), v == null ? 'cannot decode it' : barLabel(v, fr, l === 'flashraw')])),
    };
  });
  const label = `${st.metric === 'decode' ? 'RAW decode' : 'RAW to RGB'} wall time per camera in milliseconds, FlashRaw and other libraries, linear scale from 0, ${machineName(st.machine)}`;
  const r = timeBars(root, bars, label);
  return { ...r, total: rows.length, shown: shown.length, libs };
}

/** Table cell: "182 ms" for FlashRaw, "182 (3.2×)" for the others. */
const cell = (v: number | null | undefined, fr: number | null | undefined, isFr: boolean) =>
  v == null ? 'n/a' : `${fmtMs(v)}${isFr || fr == null ? '' : ` (${fmtBar(v / fr)})`}`;

function cameraTable(st: CamState) {
  const rows = D.cameras.filter((c) => c.machine === st.machine);
  const libs = ['flashraw', ...others(st.metric)];
  return h('table', { class: 'plain' },
    h('thead', {}, h('tr', {}, h('th', { scope: 'col', text: 'Camera' }), h('th', { scope: 'col', text: 'Format · codec' }), h('th', { scope: 'col', class: 'num', text: 'MP' }), ...libs.map((l) => h('th', { scope: 'col', class: 'num', text: `${libName(l)} (ms)` })))),
    h('tbody', {}, ...rows.map((r) => h('tr', {},
      h('th', { scope: 'row', text: r.camera }), h('td', { text: `${r.format} · ${r.codec}` }), h('td', { class: 'num', text: r.mp != null ? String(r.mp) : '' }),
      ...libs.map((l) => h('td', { class: 'num', text: cell(r[st.metric][l], r[st.metric].flashraw, l === 'flashraw') })),
    ))),
  );
}

// ----------------------------------------------------------------- per format

// The most common formats first (what people shoot today), then the rest by number of files.
const COMMON_FORMATS = ['CR3', 'CR2', 'NEF', 'ARW', 'RAF', 'DNG', 'RW2', 'ORF', 'PEF', 'SRW', 'RWL', '3FR', 'IIQ'];
const formatRank = (format: string) => {
  const i = COMMON_FORMATS.indexOf(format.toUpperCase());
  return i < 0 ? COMMON_FORMATS.length : i;
};

function formatChart(root: HTMLElement, st: CamState) {
  const libs = ['flashraw', ...others(st.metric)];
  const rows = D.formats
    .map((f) => ({ f, v: f.ms[st.machine]?.[st.metric] }))
    .filter((r) => r.v?.flashraw != null)
    .sort((a, b) => formatRank(a.f.format) - formatRank(b.f.format) || (b.f.files ?? 0) - (a.f.files ?? 0) || a.f.codec.localeCompare(b.f.codec));
  const shown = st.all ? rows : rows.slice(0, 18);
  const bars: BarRow[] = shown.map(({ f, v }) => {
    const label = `${f.format} · ${f.codec}`;
    const fr = v.flashraw!;
    const values = libs.map((l) => [l, v[l] ?? null] as [string, number | null]);
    return {
      label,
      detail: `${f.files ?? '?'} file${f.files === 1 ? '' : 's'}`,
      values,
      aria: `${label}: ${values.map(([l, x]) => `${libName(l)} ${x == null ? 'cannot decode it' : barLabel(x, fr, l === 'flashraw')}`).join(', ')}`,
      tip: () => tipRows(label, `${f.files ?? '?'} file(s), geometric mean, ${machineName(st.machine)}`, values.map(([l, x]) => [l, libName(l), x == null ? 'cannot decode it' : barLabel(x, fr, l === 'flashraw')])),
    };
  });
  const r = timeBars(root, bars, `Geometric-mean wall time per format and codec in milliseconds, FlashRaw and other libraries, linear scale from 0, ${machineName(st.machine)}`);
  return { ...r, total: rows.length, shown: shown.length, libs };
}

function formatTable(st: CamState) {
  const libs = ['flashraw', ...others(st.metric)];
  return h('table', { class: 'plain' },
    h('thead', {}, h('tr', {}, h('th', { scope: 'col', text: 'Format' }), h('th', { scope: 'col', text: 'Codec' }), h('th', { scope: 'col', class: 'num', text: 'Files' }), ...libs.map((l) => h('th', { scope: 'col', class: 'num', text: `${libName(l)} (ms)` })))),
    h('tbody', {}, ...D.formats.map((f) => {
      const v = f.ms[st.machine]?.[st.metric] ?? {};
      return h('tr', {},
        h('th', { scope: 'row', text: f.format }), h('td', { text: f.codec }), h('td', { class: 'num', text: String(f.files ?? '') }),
        ...libs.map((l) => h('td', { class: 'num', text: cell(v[l], v.flashraw, l === 'flashraw') })),
      );
    })),
  );
}

// ----------------------------------------------------------------- assembly

const tableBox = () => h('details', { class: 'chart-table' }, h('summary', { text: 'Show the data as a table' }), h('div', { class: 'table-scroll', tabindex: '0' }));

export function renderCharts(root: HTMLElement) {
  const st: CamState = { machine: D.machines.find((m) => m.id === 'macos')?.id ?? D.machines[D.machines.length - 1]?.id ?? 'macos', metric: 'rgb', all: false };
  const fst: CamState = { ...st };
  root.textContent = '';
  root.dataset.state = 'ready';
  root.append(headlineTiles());
  const machineOpts = D.machines.map((m) => [m.id, m.name] as [string, string]);
  const cut = (max: number) => `axis cut at ${max.toLocaleString('en-US')} ms: longer bars are marked and labelled`;

  // camera chart card
  const camBody = h('div', { class: 'chart-body' });
  const camLegend = h('div');
  const camMore = h('button', { type: 'button', class: 'btn btn-outline btn-sm' }) as HTMLButtonElement;
  const camTable = tableBox();
  const camSub = h('p');
  const drawCam = () => {
    const r = cameraChart(camBody, st);
    camBody.replaceChildren(r.svg);
    camLegend.replaceChildren(legend(r.libs));
    camMore.textContent = st.all ? 'Show fewer cameras' : `Show all ${r.total} cameras`;
    camMore.hidden = r.total <= r.shown && !st.all;
    camSub.textContent = `${st.metric === 'decode' ? 'Open + full sensor decode to 16-bit samples' : 'Decode + demosaic + white balance + colour to 8-bit sRGB'} · ${machineName(st.machine)} · wall time in ms (lower is better), median of 7 runs, all cores · ${cut(r.max)}`;
    camTable.querySelector('.table-scroll')!.replaceChildren(cameraTable(st));
  };
  camMore.addEventListener('click', () => {
    st.all = !st.all;
    drawCam();
  });
  root.append(
    h('div', { class: 'chart-card' },
      h('div', { class: 'chart-head' },
        h('div', {}, h('h3', { text: 'Popular cameras: time per file' }), camSub),
        h('div', { class: 'chart-controls' },
          segmented('Metric', [['decode', 'RAW decode'], ['rgb', 'RAW → RGB']], st.metric, (v) => { st.metric = v as CamState['metric']; drawCam(); }),
          segmented('Machine', machineOpts, st.machine, (v) => { st.machine = v; drawCam(); }),
        ),
      ),
      camLegend,
      camBody,
      h('div', { class: 'chart-more' }, camMore),
      camTable,
    ),
  );

  // format chart card
  const fBody = h('div', { class: 'chart-body' });
  const fLegend = h('div');
  const fMore = h('button', { type: 'button', class: 'btn btn-outline btn-sm' }) as HTMLButtonElement;
  const fTable = tableBox();
  const fSub = h('p');
  const drawF = () => {
    const r = formatChart(fBody, fst);
    fBody.replaceChildren(r.svg);
    fLegend.replaceChildren(legend(r.libs));
    fMore.textContent = fst.all ? 'Show fewer' : `Show all ${r.total} format/codec pairs`;
    fMore.hidden = r.total <= r.shown && !fst.all;
    fSub.textContent = `Geometric mean of the wall times of each format's files, in ms (lower is better), all cores · ${machineName(fst.machine)} · ${cut(r.max)}`;
    fTable.querySelector('.table-scroll')!.replaceChildren(formatTable(fst));
  };
  fMore.addEventListener('click', () => {
    fst.all = !fst.all;
    drawF();
  });
  root.append(
    h('div', { class: 'chart-card' },
      h('div', { class: 'chart-head' },
        h('div', {}, h('h3', { text: 'Time per format and codec' }), fSub),
        h('div', { class: 'chart-controls' },
          segmented('Metric', [['decode', 'RAW decode'], ['rgb', 'RAW → RGB']], fst.metric, (v) => { fst.metric = v as CamState['metric']; drawF(); }),
          segmented('Machine', machineOpts, fst.machine, (v) => { fst.machine = v; drawF(); }),
        ),
      ),
      fLegend,
      fBody,
      h('div', { class: 'chart-more' }, fMore),
      fTable,
    ),
  );

  drawCam();
  drawF();
  let lastW = root.clientWidth;
  new ResizeObserver(() => {
    if (Math.abs(root.clientWidth - lastW) < 8) return;
    lastW = root.clientWidth;
    drawCam();
    drawF();
  }).observe(root);
  window.addEventListener('scroll', hideTip, { passive: true });
}
