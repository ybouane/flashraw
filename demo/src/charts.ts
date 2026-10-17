// Benchmark charts (plain SVG): headline speedups, per-camera wall times (log-scale dot plot)
// and per-format speedups. Data: `virtual:charts-data` (docs/charts/data.json or the sample).

import data from 'virtual:charts-data';
import type { ChartsData } from './charts-schema';
import { h } from './metadata';

const D = data as ChartsData;
const SVGNS = 'http://www.w3.org/2000/svg';
// Monochrome series: FlashRaw solid ink, LibRaw solid grey, RawSpeed hollow (shape tells them apart).
const COLORS: Record<string, string> = { flashraw: '#0e0e0e', libraw: '#9a9a9a', rawspeed: '#5c5c5c' };
const HOLLOW = new Set(['rawspeed']);
const color = (l: string) => COLORS[l] ?? '#7a7a7a';
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
      key.style.background = HOLLOW.has(lib) ? 'transparent' : lib === 'flashraw' ? '#fff' : color(lib);
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

function keyStyle(el: HTMLElement, lib: string) {
  el.style.borderRadius = '50%';
  if (HOLLOW.has(lib)) {
    el.style.background = '#fff';
    el.style.boxShadow = `inset 0 0 0 1.5px ${color(lib)}`;
  } else {
    el.style.background = color(lib);
  }
}

function legend(items: [string, string][]) {
  return h('ul', { class: 'legend' }, ...items.map(([lib, label]) => {
    const k = h('span', { class: 'key' });
    keyStyle(k, lib);
    return h('li', {}, k, label);
  }));
}

function marker(lib: string, cx: number, cy: number, big = false) {
  const r = big ? 5.5 : 4.5;
  return HOLLOW.has(lib)
    ? s('circle', { cx, cy, r, fill: '#fff', stroke: color(lib), 'stroke-width': 1.5 })
    : s('circle', { cx, cy, r, fill: color(lib), stroke: '#fff', 'stroke-width': 1.5 });
}

// ----------------------------------------------------------------- log axis

function logScale(min: number, max: number, x0: number, x1: number) {
  const a = Math.log10(min);
  const b = Math.log10(max);
  return (v: number) => x0 + ((Math.log10(Math.max(min, Math.min(max, v))) - a) / (b - a)) * (x1 - x0);
}

function decades(min: number, max: number, withHalves: boolean): number[] {
  const out: number[] = [];
  for (let e = Math.floor(Math.log10(min)); e <= Math.ceil(Math.log10(max)); e++) {
    for (const m of withHalves ? [1, 2, 5] : [1]) {
      const v = m * 10 ** e;
      if (v >= min * 0.999 && v <= max * 1.001) out.push(v);
    }
  }
  return out;
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

// ----------------------------------------------------------------- per-camera dot plot

interface CamState { machine: string; metric: 'decode' | 'rgb'; all: boolean }

function cameraChart(root: HTMLElement, st: CamState) {
  const rows = D.cameras.filter((c) => c.machine === st.machine);
  const libs = (st.metric === 'decode' ? ['flashraw', 'libraw', 'rawspeed'] : ['flashraw', 'libraw']).filter((l) => rows.some((r) => r[st.metric][l] != null));
  const shown = st.all ? rows : rows.slice(0, 18);
  const width = Math.max(320, root.clientWidth || 900);
  const narrow = width < 640;
  const labelW = narrow ? 0 : Math.min(250, width * 0.3);
  const rightW = narrow ? 64 : 92;
  const rowH = narrow ? 44 : 30;
  const top = 26;
  const x0 = labelW + 8;
  const x1 = width - rightW - 10;
  const values = rows.flatMap((r) => libs.map((l) => r[st.metric][l]).filter((v): v is number => v != null));
  const min = Math.max(0.5, 10 ** Math.floor(Math.log10(Math.min(...values))));
  const max = 10 ** Math.ceil(Math.log10(Math.max(...values)));
  const x = logScale(min, max, x0, x1);
  const height = top + shown.length * rowH + 8;
  const svg = s('svg', { class: 'chart-svg', viewBox: `0 0 ${width} ${height}`, width, height, role: 'group', 'aria-label': `${st.metric === 'decode' ? 'RAW decode' : 'RAW to RGB'} wall time per camera, milliseconds, log scale, ${machineName(st.machine)}` });
  // axis
  for (const t of decades(min, max, true)) {
    const major = Math.abs(Math.log10(t) - Math.round(Math.log10(t))) < 1e-9;
    svg.append(s('line', { class: 'grid', x1: x(t), x2: x(t), y1: top - 6, y2: height - 8, 'stroke-opacity': major ? 1 : 0.55 }));
    if (major || !narrow) svg.append(s('text', { x: x(t), y: 12, 'text-anchor': 'middle', class: 't-muted' }, `${t >= 1000 ? `${t / 1000}k` : t}`));
  }
  if (!narrow) svg.append(s('text', { x: 0, y: 12, class: 't-muted' }, 'Wall time (ms, log scale)'));
  svg.append(s('text', { x: width - 4, y: 12, 'text-anchor': 'end', class: 't-muted' }, `vs ${libName('libraw')}`));

  shown.forEach((r, i) => {
    const y = top + i * rowH + (narrow ? 30 : rowH / 2);
    const g = s('g', { class: 'row', tabindex: 0, role: 'img' });
    const vals = libs.map((l) => [l, r[st.metric][l]] as const);
    const fr = r[st.metric].flashraw;
    const lr = r[st.metric].libraw;
    const sp = fr && lr ? lr / fr : null;
    g.setAttribute('aria-label', `${r.camera}, ${r.format} ${r.codec}: ${vals.map(([l, v]) => `${libName(l)} ${v == null ? 'not supported' : `${fmtMs(v)} ms`}`).join(', ')}`);
    g.append(s('rect', { class: 'row-hit', x: 0, y: y - (narrow ? 30 : rowH / 2), width, height: rowH, rx: 0 }));
    if (narrow) {
      g.append(s('text', { x: 0, y: y - 14, class: 't-strong' }, r.camera));
      g.append(s('text', { x: width - 4, y: y - 14, 'text-anchor': 'end', class: 't-muted' }, `${r.format} · ${r.mp ?? '?'} MP`));
    } else {
      g.append(s('text', { x: 0, y: y + 4, class: 't-strong' }, r.camera.length > 34 ? r.camera.slice(0, 33) + '…' : r.camera));
    }
    const present = vals.filter(([, v]) => v != null) as [string, number][];
    if (present.length > 1) {
      const xs = present.map(([, v]) => x(v));
      g.append(s('line', { x1: Math.min(...xs), x2: Math.max(...xs), y1: y, y2: y, stroke: '#d6d3cc', 'stroke-width': 1.5 }));
    }
    // FlashRaw last, on top
    for (const [l, v] of [...present].sort((a) => (a[0] === 'flashraw' ? 1 : -1))) {
      g.append(marker(l, x(v), y, l === 'flashraw'));
    }
    const missing = vals.filter(([, v]) => v == null).map(([l]) => libName(l));
    if (missing.length && !narrow) {
      g.append(s('text', { x: x1, y: y + 4, 'text-anchor': 'end', class: 't-muted' }, `${missing.join(', ')}: n/a`));
    }
    g.append(s('text', { x: width - 4, y: y + 4, 'text-anchor': 'end', class: sp && sp >= 1 ? 't-num' : 't-muted' }, sp ? fmtX(sp) : '–'));
    const tipContent = () =>
      tipRows(r.camera, `${r.format} · ${r.codec}${r.mp ? ` · ${r.mp} MP` : ''}`, vals.map(([l, v]) => [l, libName(l), v == null ? 'not supported' : `${fmtMs(v)} ms${l !== 'flashraw' && fr ? ` (${fmtX(v / fr)})` : ''}`]));
    g.addEventListener('pointerenter', () => showTip(g, tipContent()));
    g.addEventListener('pointerleave', hideTip);
    g.addEventListener('focus', () => showTip(g, tipContent()));
    g.addEventListener('blur', hideTip);
    svg.append(g);
  });
  return { svg, total: rows.length, shown: shown.length, libs };
}

function cameraTable(st: CamState) {
  const rows = D.cameras.filter((c) => c.machine === st.machine);
  const libs = st.metric === 'decode' ? ['flashraw', 'libraw', 'rawspeed'] : ['flashraw', 'libraw'];
  return h('table', { class: 'plain' },
    h('thead', {}, h('tr', {}, h('th', { scope: 'col', text: 'Camera' }), h('th', { scope: 'col', text: 'Format · codec' }), h('th', { scope: 'col', text: 'MP' }), ...libs.map((l) => h('th', { scope: 'col', text: `${libName(l)} (ms)` })))),
    h('tbody', {}, ...rows.map((r) => h('tr', {},
      h('td', { text: r.camera }), h('td', { text: `${r.format} · ${r.codec}` }), h('td', { class: 'num', text: r.mp != null ? String(r.mp) : '' }),
      ...libs.map((l) => h('td', { class: 'num', text: r[st.metric][l] != null ? fmtMs(r[st.metric][l]!) : 'n/a' })),
    ))),
  );
}

// ----------------------------------------------------------------- per-format speedups

function formatChart(root: HTMLElement, st: CamState) {
  const series = st.metric === 'decode'
    ? ([['decodeVsLibraw', 'libraw'], ['decodeVsRawspeed', 'rawspeed']] as const)
    : ([['rgbVsLibraw', 'libraw']] as const);
  const rows = D.formats
    .map((f) => ({ f, v: f.speedup[st.machine] }))
    .filter((r) => r.v && series.some(([k]) => r.v[k] != null))
    .sort((a, b) => (b.v[series[0][0]] ?? 0) - (a.v[series[0][0]] ?? 0));
  const shown = st.all ? rows : rows.slice(0, 18);
  const width = Math.max(320, root.clientWidth || 900);
  const narrow = width < 640;
  const labelW = narrow ? 118 : 200;
  const rowH = 26;
  const top = 26;
  const x0 = labelW + 8;
  const x1 = width - 16;
  const all = rows.flatMap((r) => series.map(([k]) => r.v[k]).filter((v): v is number => v != null));
  const min = Math.min(0.2, 10 ** Math.floor(Math.log10(Math.min(...all))));
  const max = 10 ** Math.ceil(Math.log10(Math.max(...all)));
  const x = logScale(min, max, x0, x1);
  const height = top + shown.length * rowH + 8;
  const svg = s('svg', { class: 'chart-svg', viewBox: `0 0 ${width} ${height}`, width, height, role: 'group', 'aria-label': `Geometric-mean speedup of FlashRaw per format and codec, log scale, ${machineName(st.machine)}` });
  // slower / faster shading
  svg.append(s('rect', { x: x0, y: top - 6, width: x(1) - x0, height: height - top, fill: '#f6f4ef' }));
  for (const t of decades(min, max, true)) {
    svg.append(s('line', { class: 'grid', x1: x(t), x2: x(t), y1: top - 6, y2: height - 8 }));
    if (!narrow || [0.1, 1, 10, 100].some((m) => Math.abs(m - t) < 1e-9)) svg.append(s('text', { x: x(t), y: 12, 'text-anchor': 'middle', class: 't-muted' }, `${t}×`));
  }
  svg.append(s('line', { class: 'ref', x1: x(1), x2: x(1), y1: top - 6, y2: height - 4 }));
  svg.append(s('text', { x: x(1) - 6, y: height - 2, 'text-anchor': 'end', class: 't-muted' }, '← other faster'));
  svg.append(s('text', { x: x(1) + 6, y: height - 2, class: 't-muted' }, 'FlashRaw faster →'));
  shown.forEach(({ f, v }, i) => {
    const y = top + i * rowH + rowH / 2;
    const g = s('g', { class: 'row', tabindex: 0, role: 'img' });
    const label = `${f.format} · ${f.codec}`;
    g.setAttribute('aria-label', `${label}: ${series.map(([k, l]) => `vs ${libName(l)} ${v[k] == null ? 'n/a' : fmtX(v[k]!)}`).join(', ')}`);
    g.append(s('rect', { class: 'row-hit', x: 0, y: y - rowH / 2, width, height: rowH }));
    g.append(s('text', { x: 0, y: y + 4, class: 't-strong' }, narrow && label.length > 18 ? label.slice(0, 17) + '…' : label));
    for (const [k, l] of series) {
      const val = v[k];
      if (val == null) continue;
      g.append(marker(l, x(val), y));
    }
    const content = () => tipRows(label, `${f.files ?? '?'} file(s), geometric mean, ${machineName(st.machine)}`, series.map(([k, l]) => [l, `vs ${libName(l)}`, v[k] == null ? 'not decoded by it' : fmtX(v[k]!)]));
    g.addEventListener('pointerenter', () => showTip(g, content()));
    g.addEventListener('pointerleave', hideTip);
    g.addEventListener('focus', () => showTip(g, content()));
    g.addEventListener('blur', hideTip);
    svg.append(g);
  });
  return { svg, total: rows.length, shown: shown.length, series };
}

function formatTable(st: CamState) {
  const cols = st.metric === 'decode' ? ([['decodeVsLibraw', 'libraw'], ['decodeVsRawspeed', 'rawspeed']] as const) : ([['rgbVsLibraw', 'libraw']] as const);
  return h('table', { class: 'plain' },
    h('thead', {}, h('tr', {}, h('th', { scope: 'col', text: 'Format' }), h('th', { scope: 'col', text: 'Codec' }), h('th', { scope: 'col', text: 'Files' }), ...cols.map(([, l]) => h('th', { scope: 'col', text: `vs ${libName(l)}` })))),
    h('tbody', {}, ...D.formats.map((f) => h('tr', {},
      h('td', { text: f.format }), h('td', { text: f.codec }), h('td', { class: 'num', text: String(f.files ?? '') }),
      ...cols.map(([k]) => h('td', { class: 'num', text: f.speedup[st.machine]?.[k] != null ? fmtX(f.speedup[st.machine][k]!) : 'n/a' })),
    ))),
  );
}

// ----------------------------------------------------------------- assembly

export function renderCharts(root: HTMLElement) {
  const st: CamState = { machine: D.machines[D.machines.length - 1]?.id ?? 'macos', metric: 'decode', all: false };
  const fst: CamState = { ...st };
  root.textContent = '';
  root.dataset.state = 'ready';
  root.append(headlineTiles());

  // camera chart card
  const camBody = h('div');
  const camLegend = h('div');
  const camMore = h('button', { type: 'button', class: 'btn btn-outline btn-sm' }) as HTMLButtonElement;
  const camTable = h('details', { class: 'chart-table' }, h('summary', { text: 'Show the data as a table' }), h('div', { class: 'table-scroll' }));
  const camSub = h('p');
  const drawCam = () => {
    const r = cameraChart(camBody, st);
    camBody.replaceChildren(r.svg);
    camLegend.replaceChildren(legend(r.libs.map((l) => [l, libName(l)])));
    camMore.textContent = st.all ? 'Show fewer cameras' : `Show all ${r.total} cameras`;
    camMore.hidden = r.total <= r.shown && !st.all;
    camSub.textContent = `${st.metric === 'decode' ? 'Open + full sensor decode to 16-bit samples' : 'Decode + demosaic + white balance + colour to 8-bit sRGB'} · ${machineName(st.machine)} · ms, log scale, lower is better · right column: speedup vs ${libName('libraw')}`;
    camTable.querySelector('.table-scroll')!.replaceChildren(cameraTable(st));
  };
  camMore.addEventListener('click', () => {
    st.all = !st.all;
    drawCam();
  });
  const machineOpts = D.machines.map((m) => [m.id, m.name] as [string, string]);
  root.append(
    h('div', { class: 'chart-card' },
      h('div', { class: 'chart-head' },
        h('div', {}, h('h3', { text: 'Popular cameras: wall time per file' }), camSub),
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
  const fBody = h('div');
  const fLegend = h('div');
  const fMore = h('button', { type: 'button', class: 'btn btn-outline btn-sm' }) as HTMLButtonElement;
  const fTable = h('details', { class: 'chart-table' }, h('summary', { text: 'Show the data as a table' }), h('div', { class: 'table-scroll' }));
  const fSub = h('p');
  const drawF = () => {
    const r = formatChart(fBody, fst);
    fBody.replaceChildren(r.svg);
    fLegend.replaceChildren(legend(r.series.map(([, l]) => [l, `vs ${libName(l)}`])));
    fMore.textContent = fst.all ? 'Show fewer' : `Show all ${r.total} format/codec pairs`;
    fMore.hidden = r.total <= r.shown && !fst.all;
    fSub.textContent = `Geometric mean of (other library time ÷ FlashRaw time), all cores · ${machineName(fst.machine)} · right of the dashed line: FlashRaw faster`;
    fTable.querySelector('.table-scroll')!.replaceChildren(formatTable(fst));
  };
  fMore.addEventListener('click', () => {
    fst.all = !fst.all;
    drawF();
  });
  root.append(
    h('div', { class: 'chart-card' },
      h('div', { class: 'chart-head' },
        h('div', {}, h('h3', { text: 'Speedup per format and codec' }), fSub),
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
  if (D.sample) {
    root.append(h('p', { class: 'fineprint', text: `Data: ${D.source ?? 'docs/benchmarks.md'}.` }));
  }

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
