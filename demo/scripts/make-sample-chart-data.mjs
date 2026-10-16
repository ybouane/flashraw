// Regenerates src/charts-data.sample.json from the tables in docs/benchmarks.md.
// The real chart data (docs/charts/data.json) uses the same schema (see README.md, "Chart
// data"); when it exists the build uses it instead of the sample.
//
//   node scripts/make-sample-chart-data.mjs

import { readFileSync, writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('../../', import.meta.url));
const md = readFileSync(root + 'docs/benchmarks.md', 'utf8');

const num = (s) => {
  s = s.replace(/\*/g, '').replace(/×/g, '').trim();
  if (s === '❌' || s === '' || s === '–') return null;
  const v = Number(s);
  return Number.isFinite(v) ? v : null;
};

/** Rows of the first markdown table after `heading`. */
function table(heading) {
  const at = md.indexOf(heading);
  if (at < 0) throw new Error(`missing section ${heading}`);
  const lines = md.slice(at).split('\n');
  const rows = [];
  let started = false;
  for (const l of lines) {
    if (l.startsWith('|')) {
      started = true;
      rows.push(l.slice(1, -1).split('|').map((c) => c.trim()));
    } else if (started) break;
  }
  return rows.slice(2); // header + separator
}

function cameras(heading, machine) {
  return table(heading).map((c) => {
    const [format, codec] = c[1].split('·').map((s) => s.trim());
    const row = {
      machine,
      camera: c[0],
      format,
      codec,
      mp: num(c[2]),
      decode: { flashraw: num(c[3]), libraw: num(c[4]), rawspeed: num(c[5]) },
      rgb: { flashraw: num(c[6]), libraw: num(c[7]) },
    };
    if (c[8] !== undefined) row.gpu = num(c[8]);
    return row;
  });
}

const headline = [];
for (const c of table('## Headline')) {
  const [, metricText, vsText] = c[0].match(/^(decode|RAW → RGB) vs (\w+)$/) ?? [];
  const metric = metricText === 'decode' ? 'decode' : 'rgb';
  const vs = vsText.toLowerCase();
  const threads = c[1] === 'all cores' ? 'all' : '1';
  const cols = [
    ['linux', 'popular', c[2]],
    ['linux', 'all', c[3]],
    ['macos', 'popular', c[4]],
    ['macos', 'all', c[5]],
  ];
  for (const [machine, set, cell] of cols) {
    // "**6.14×** / 8.26× · 57/59"
    const m = cell.replace(/\*/g, '').match(/([\d.]+)× \/ ([\d.]+)× · (\d+)\/(\d+)/);
    headline.push({ metric, vs, threads, set, machine, geomean: +m[1], median: +m[2], wins: +m[3], files: +m[4] });
  }
}

const formats = table('## Per format').map((c) => ({
  format: c[0],
  codec: c[1],
  files: num(c[2]),
  speedup: {
    linux: { decodeVsLibraw: num(c[3]), decodeVsRawspeed: num(c[4]), rgbVsLibraw: num(c[5]) },
    macos: { decodeVsLibraw: num(c[6]), decodeVsRawspeed: num(c[7]), rgbVsLibraw: num(c[8]) },
  },
}));

const data = {
  version: 1,
  sample: true,
  source: 'docs/benchmarks.md',
  description: 'Times in ms (lower is better), all cores, median of runs; speedups = other library time / FlashRaw time.',
  libraries: [
    { id: 'flashraw', name: 'FlashRaw' },
    { id: 'libraw', name: 'LibRaw' },
    { id: 'rawspeed', name: 'RawSpeed' },
  ],
  machines: [
    { id: 'linux', name: 'Linux x86-64', cpu: 'AMD Ryzen 7 9700X, 16 threads', icon: '🐧' },
    { id: 'macos', name: 'macOS Apple Silicon', cpu: 'Apple M1 Ultra, 20 cores', icon: '🍎' },
  ],
  headline,
  cameras: [
    ...cameras('## Popular cameras (Linux x86-64)', 'linux'),
    ...cameras('## Popular cameras (Apple M1 Ultra)', 'macos'),
  ],
  formats,
};

writeFileSync(new URL('../src/charts-data.sample.json', import.meta.url), JSON.stringify(data, null, 1) + '\n');
console.log(`headline ${headline.length}, cameras ${data.cameras.length}, formats ${formats.length}`);
