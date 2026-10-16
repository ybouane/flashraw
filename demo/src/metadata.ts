// Metadata cards: camera, lens, exposure, image, sensor/CFA, levels, white balance, colour,
// DNG, GPS, vendor extras.

import type { RawMetadata, FrameInfo } from 'flashraw';

type Row = [string, string | Node | null | undefined];

export function h<K extends keyof HTMLElementTagNameMap>(tag: K, attrs: Record<string, string> = {}, ...kids: (Node | string | null | undefined)[]) {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (k === 'class') el.className = v;
    else if (k === 'text') el.textContent = v;
    else el.setAttribute(k, v);
  }
  for (const k of kids) if (k != null) el.append(k);
  return el;
}

function kv(rows: Row[]): HTMLDListElement {
  const dl = h('dl', { class: 'kv' });
  for (const [k, v] of rows) {
    if (v === null || v === undefined || v === '') continue;
    dl.append(h('dt', { text: k }), h('dd', {}, v));
  }
  return dl;
}

function card(title: string, ...kids: (Node | null)[]): HTMLElement | null {
  const content = kids.filter((k): k is Node => !!k && !(k instanceof HTMLDListElement && !k.childElementCount));
  if (!content.length) return null;
  return h('article', { class: 'card meta-card' }, h('h4', { text: title }), ...content);
}

const fmt = (n: number | null | undefined, digits = 2) =>
  n === null || n === undefined || !Number.isFinite(n) ? null : String(+n.toFixed(digits));

export function shutter(t: number | null | undefined): string | null {
  if (!t || t <= 0) return null;
  if (t >= 0.3) return `${+t.toFixed(1)} s`;
  return `1/${Math.round(1 / t)} s`;
}

const ORIENT: Record<number, string> = {
  1: 'Normal',
  2: 'Mirrored horizontally',
  3: 'Rotated 180°',
  4: 'Mirrored vertically',
  5: 'Transposed',
  6: 'Rotate 90° CW',
  7: 'Transversed',
  8: 'Rotate 90° CCW',
};
export const orientationName = (o: number) => `${ORIENT[o] ?? o} (${o})`;

const METERING: Record<number, string> = { 0: 'Unknown', 1: 'Average', 2: 'Centre-weighted', 3: 'Spot', 4: 'Multi-spot', 5: 'Multi-segment', 6: 'Partial' };
const PROGRAM: Record<number, string> = { 1: 'Manual', 2: 'Program', 3: 'Aperture priority', 4: 'Shutter priority', 5: 'Creative', 6: 'Action', 7: 'Portrait', 8: 'Landscape' };
const ILLUMINANT: Record<number, string> = { 1: 'Daylight', 2: 'Fluorescent', 3: 'Tungsten', 4: 'Flash', 9: 'Fine weather', 10: 'Cloudy', 11: 'Shade', 17: 'Standard A', 18: 'Standard B', 19: 'Standard C', 20: 'D55', 21: 'D65', 22: 'D75', 23: 'D50', 24: 'ISO studio tungsten' };

export function bytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 ** 2) return `${(n / 1024).toFixed(1)} KiB`;
  if (n < 1024 ** 3) return `${(n / 1024 ** 2).toFixed(1)} MiB`;
  return `${(n / 1024 ** 3).toFixed(2)} GiB`;
}

function cfaGrid(m: RawMetadata): Node | null {
  const c = m.cfa;
  if (!c) return null;
  const g = h('span', { class: 'cfa', 'aria-hidden': 'true' });
  g.style.gridTemplateColumns = `repeat(${c.width}, 14px)`;
  for (const ch of c.colors) g.append(h('i', { class: `cfa-${ch}`, text: ch }));
  const wrap = h('span', {}, g, h('span', { text: c.pattern.length > 8 ? `${c.width}×${c.height} ${m.layout}` : c.pattern }));
  wrap.style.display = 'inline-flex';
  wrap.style.alignItems = 'center';
  return wrap;
}

function wbBars(wb: number[]): Node {
  const g = wb[1] || 1;
  const norm = wb.slice(0, 3).map((v) => v / g);
  const max = Math.max(...norm, 1);
  const colors = ['var(--sensor-r)', 'var(--sensor-g)', 'var(--sensor-b)'];
  const wrap = h('div', { class: 'wb-bars' });
  ['R', 'G', 'B'].forEach((label, i) => {
    const bar = h('span', { class: 'track' }, h('i'));
    const fill = bar.firstChild as HTMLElement;
    fill.style.width = `${(norm[i] / max) * 100}%`;
    fill.style.background = colors[i];
    wrap.append(h('div', { class: 'wb-bar' }, h('span', { text: label }), bar, h('span', { text: norm[i].toFixed(3) })));
  });
  return wrap;
}

function pill(text: string, kind = '') {
  return h('span', { class: `pill ${kind}`, text });
}

export function renderMetadata(root: HTMLElement, m: RawMetadata, info: FrameInfo, file: File) {
  root.textContent = '';
  const e = m.exposure;
  const L = m.lens;
  const range = (r: [number, number] | null, unit: string, prefix = '') =>
    r ? (r[0] === r[1] ? `${prefix}${fmt(r[0], 1)}${unit}` : `${prefix}${fmt(r[0], 1)}–${fmt(r[1], 1)}${unit}`) : null;

  const cards: (HTMLElement | null)[] = [
    card(
      'Camera',
      h('p', { class: 'meta-hero', text: m.camera.name || `${m.camera.make} ${m.camera.model}` }),
      kv([
        ['Make', m.camera.make],
        ['Model', m.camera.model],
        ['Canonical', m.camera.canonicalMake && `${m.camera.canonicalMake} ${m.camera.canonicalModel}`],
        ['Serial', m.camera.serial],
        ['Firmware', m.camera.firmware],
        ['Software', m.software],
        ['Artist', m.artist],
        ['Copyright', m.copyright],
      ]),
    ),
    card(
      'Lens',
      kv([
        ['Model', L.model ?? null],
        ['Make', L.make],
        ['Focal range', range(L.focalRange, ' mm')],
        ['Aperture', range(L.apertureRange, '', 'f/')],
        ['Serial', L.serial],
      ]),
    ),
    card(
      'Exposure',
      kv([
        ['Shutter', shutter(e.exposureTime)],
        ['Aperture', e.fNumber ? `f/${fmt(e.fNumber, 1)}` : null],
        ['ISO', e.iso ? String(e.iso) : null],
        ['Focal length', e.focalLength ? `${fmt(e.focalLength, 1)} mm${e.focalLength35mm ? ` (${fmt(e.focalLength35mm, 0)} mm eq.)` : ''}` : null],
        ['Exposure bias', e.exposureBias != null ? `${e.exposureBias > 0 ? '+' : ''}${fmt(e.exposureBias, 2)} EV` : null],
        ['Program', e.exposureProgram != null ? (PROGRAM[e.exposureProgram] ?? String(e.exposureProgram)) : null],
        ['Metering', e.meteringMode != null ? (METERING[e.meteringMode] ?? String(e.meteringMode)) : null],
        ['Flash', e.flash != null ? (e.flash & 1 ? 'Fired' : 'Did not fire') : null],
        ['Captured', m.captureTime ? m.captureTime.replace('T', ' ').replace(/\.\d+/, '') : null],
      ]),
    ),
    card(
      'Image',
      kv([
        ['Format', h('span', {}, pill(m.format), ' ', m.dngVersion ? `DNG ${m.dngVersion}` : '')],
        ['Compression', h('code', { text: m.compression })],
        ['Size', `${m.width} × ${m.height} (${((m.width * m.height) / 1e6).toFixed(1)} MP)`],
        ['Sensor buffer', `${m.rawWidth} × ${m.rawHeight}`],
        ['Bits per sample', String(m.bitsPerSample)],
        ['Pixel aspect', info.pixelAspect !== 1 ? fmt(info.pixelAspect, 4) : null],
        ['Orientation', orientationName(m.orientation)],
        ['Orientation from', m.orientationInfo ? `${m.orientationInfo.source}${m.orientationInfo.autoRotate === false ? ' (auto-rotate off)' : ''}` : null],
        ['File', `${file.name} · ${bytes(file.size)}`],
      ]),
    ),
    card(
      'Sensor',
      kv([
        ['Layout', m.layout],
        ['CFA', cfaGrid(m)],
        ['Active area', `${info.activeArea.width} × ${info.activeArea.height} at (${info.activeArea.x}, ${info.activeArea.y})`],
        ['Default crop', `${info.crop.width} × ${info.crop.height} at (${info.crop.x}, ${info.crop.y})`],
        ['Masked areas', info.maskedAreas.length ? String(info.maskedAreas.length) : null],
        ['Sample format', info.sampleFormat],
      ]),
    ),
    card(
      'Levels',
      kv([
        ['Black', `${fmt(m.blackLevel.mean, 1)}${m.blackLevel.values.length > 1 && m.blackLevel.values.length <= 8 ? ` (${m.blackLevel.values.map((v) => fmt(v, 1)).join(', ')})` : ''}`],
        ['Black pattern', m.blackLevel.values.length > 1 ? `${m.blackLevel.cols}×${m.blackLevel.rows}` : null],
        ['Black from masked', info.blackFromMasked ? 'yes' : null],
        ['White', m.whiteLevel.length ? [...new Set(m.whiteLevel)].join(', ') : null],
        ['Dynamic range', m.whiteLevel.length ? `${fmt(Math.log2(Math.max(1, Math.max(...m.whiteLevel) - m.blackLevel.mean)), 2)} stops (code values)` : null],
      ]),
    ),
    card(
      'White balance',
      m.whiteBalance ? wbBars(m.whiteBalance) : h('p', { class: 'muted small', text: 'No as-shot multipliers.' }),
      kv([
        ['Multipliers', m.whiteBalance ? m.whiteBalance.map((v) => fmt(v, 4)).join(', ') : null],
        ['Applied by camera', m.whiteBalanceApplied ? pill('yes', 'pill-warn') : null],
        ['As-shot neutral', m.dng?.asShotNeutral ? m.dng.asShotNeutral.map((v) => fmt(v, 4)).join(', ') : null],
        ['As-shot white xy', m.dng?.asShotWhiteXY ? m.dng.asShotWhiteXY.map((v) => fmt(v, 4)).join(', ') : null],
      ]),
    ),
    card(
      'Colour',
      kv([
        ['Matrices from', m.colorMatrices.source],
        ['Colour matrices', m.colorMatrices.color.length ? m.colorMatrices.color.map((c) => ILLUMINANT[c.illuminant] ?? `illuminant ${c.illuminant}`).join(', ') : null],
        ['Forward matrices', m.colorMatrices.forward.length ? String(m.colorMatrices.forward.length) : null],
        ['Calibration', m.colorMatrices.calibration.length ? String(m.colorMatrices.calibration.length) : null],
        ['Analog balance', m.colorMatrices.analogBalance?.length ? m.colorMatrices.analogBalance.map((v) => fmt(v, 3)).join(', ') : null],
      ]),
    ),
  ];

  if (m.dng) {
    const d = m.dng;
    const lists = m.opcodes.length
      ? h('ul', { class: 'small', style: 'margin:6px 0 0;padding-left:18px' }, ...m.opcodes.map((o) => h('li', { text: `List ${o.list}: ${o.name}${o.applied ? '' : ' (not applied)'}${o.optional ? ', optional' : ''}` })))
      : null;
    cards.push(
      card(
        'DNG',
        kv([
          ['Version', m.dngVersion],
          ['Profile', d.profile?.name],
          ['Baseline exposure', d.baselineExposure != null ? `${fmt(d.baselineExposure, 2)} EV` : null],
          ['Baseline noise', fmt(d.baselineNoise, 3)],
          ['Baseline sharpness', fmt(d.baselineSharpness, 3)],
          ['Linear response limit', fmt(d.linearResponseLimit, 3)],
          ['Default scale', d.defaultScale ? d.defaultScale.map((v) => fmt(v, 3)).join(' × ') : null],
          ['Hue/sat map', d.profile?.hueSatMapDims ? d.profile.hueSatMapDims.join('×') : null],
          ['Look table', d.profile?.lookTableDims ? d.profile.lookTableDims.join('×') : null],
          ['Tone curve', d.profile?.toneCurve ? 'yes' : null],
          ['Noise profile', d.noiseProfile ? `${d.noiseProfile.length} values` : null],
          ['Opcodes', m.opcodes.length ? String(m.opcodes.length) : null],
        ]),
        lists,
      ),
    );
  }
  if (m.gps && (m.gps.latitude != null || m.gps.longitude != null)) {
    const g = m.gps;
    cards.push(
      card(
        'GPS',
        kv([
          ['Latitude', fmt(g.latitude, 6)],
          ['Longitude', fmt(g.longitude, 6)],
          ['Altitude', g.altitude != null ? `${fmt(g.altitude, 1)} m` : null],
          ['Time', g.timestamp],
        ]),
      ),
    );
  }
  const extra = Object.entries(m.extra ?? {});
  if (extra.length) {
    cards.push(card('Vendor extras', kv(extra.slice(0, 40).map(([k, v]) => [k, String(v).slice(0, 160)]))));
  }
  for (const c of cards) if (c) root.append(c);
}
