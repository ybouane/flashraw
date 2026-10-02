#!/usr/bin/env node
// FlashRaw Node.js demo (WASM in Node): print metadata, extract the embedded preview, decode,
// and report timings.
//
//   node examples/node/flashraw-info.mjs photo.dng [--preview out.jpg] [--decode out.ppm]
//        [--output raw-u16|rgba8|linear-rgb] [--json]
//
// --decode writes a 16-bit PGM for raw-u16 and an 8-bit PPM for rgba8 (alpha dropped).
// In your project: `import { FlashRaw } from 'flashraw'`.
import { writeFile } from 'node:fs/promises';
import { FlashRaw, getCapabilities } from '../../dist/node.js';

const args = process.argv.slice(2);
const opt = (k) => {
  const i = args.indexOf(k);
  return i >= 0 ? args[i + 1] : undefined;
};
const file = args.find((a, i) => !a.startsWith('--') && !['--preview', '--decode', '--output'].includes(args[i - 1]));
if (!file) {
  console.error('usage: flashraw-info.mjs <raw file> [--preview out.jpg] [--decode out.ppm] [--output rgba8] [--json]');
  process.exit(2);
}

const t0 = performance.now();
const caps = await getCapabilities();
const tInit = performance.now() - t0;
let raw;
try {
  raw = await FlashRaw.openFile(file);
} catch (e) {
  console.error(`${file}: ${e.name} [${e.kind}] ${e.message}`);
  process.exit(1);
}
const tOpen = performance.now() - t0 - tInit;
const m = raw.metadata;

if (args.includes('--json')) {
  console.log(JSON.stringify({ metadata: m, frameInfo: raw.frameInfo, capabilities: raw.capabilities }, null, 2));
} else {
  const e = m.exposure;
  const lines = [
    ['File', file],
    ['Format', `${m.format}${m.dngVersion ? ` ${m.dngVersion}` : ''} (${m.compression})`],
    ['Camera', `${m.camera.name}   [make="${m.camera.make}" model="${m.camera.model}"]`],
    ['Lens', m.lens.model],
    ['Exposure', [e.iso && `ISO ${e.iso}`, e.exposureTime && (e.exposureTime < 1 ? `1/${Math.round(1 / e.exposureTime)}s` : `${e.exposureTime}s`), e.fNumber && `f/${e.fNumber}`, e.focalLength && `${e.focalLength}mm`].filter(Boolean).join(' ')],
    ['Captured', m.captureTime],
    ['Size', `${m.width}x${m.height} (sensor ${m.rawWidth}x${m.rawHeight}, ${m.bitsPerSample} bit)`],
    ['Layout', `${m.layout} ${m.cfa?.pattern ?? ''}`],
    ['Black/White', `${m.blackLevel.values.join(' ')} / ${m.whiteLevel.join(' ')}`],
    ['WB (as shot)', m.whiteBalance?.map((x) => x.toFixed(4)).join(' ')],
    ['Orientation', m.orientation],
    ...m.previews.map((p) => ['Preview', `${p.kind} ${p.width}x${p.height} ${p.encoding} (${p.byteLength} bytes)`]),
    ['Random access', JSON.stringify(raw.capabilities.randomAccess)],
    ['WASM', `${caps.wasm.variant} (${caps.simd.level}), init ${tInit.toFixed(1)} ms, open ${tOpen.toFixed(1)} ms`],
  ];
  for (const [k, v] of lines) if (v !== null && v !== undefined && v !== '') console.log(`  ${(k + ':').padEnd(15)}${v}`);
}

const previewOut = opt('--preview');
if (previewOut) {
  const t = performance.now();
  const p = await raw.preview();
  if (!p) console.log('  (no embedded preview)');
  else {
    await writeFile(previewOut, p.data);
    console.log(`  Preview -> ${previewOut} (${p.mimeType}, ${p.width}x${p.height}, ${(performance.now() - t).toFixed(2)} ms)`);
  }
}

const output = opt('--output') ?? (opt('--decode')?.endsWith('.ppm') ? 'rgba8' : 'raw-u16');
const frame = await raw.decode({ output });
console.log(
  `  Decode:        ${output} ${frame.width}x${frame.height}x${frame.channels} in ${frame.timings.wallMs.toFixed(1)} ms ` +
    `(${frame.backend.decode}; ${Object.entries(frame.timings.stages).map(([k, v]) => `${k} ${v.toFixed(1)}`).join(', ')}; copy-out ${frame.timings.copyMs.toFixed(1)})`,
);
const decodeOut = opt('--decode');
if (decodeOut) {
  let header;
  let body;
  if (output === 'rgba8') {
    header = `P6\n${frame.width} ${frame.height}\n255\n`;
    body = Buffer.alloc(frame.width * frame.height * 3);
    for (let i = 0, j = 0; i < frame.data.length; i += 4, j += 3) {
      body[j] = frame.data[i];
      body[j + 1] = frame.data[i + 1];
      body[j + 2] = frame.data[i + 2];
    }
  } else if (output === 'raw-u16' && frame.channels === 1) {
    header = `P5\n${frame.width} ${frame.height}\n65535\n`;
    body = Buffer.alloc(frame.data.length * 2);
    for (let i = 0; i < frame.data.length; i++) body.writeUInt16BE(frame.data[i], i * 2); // PGM is big-endian
  } else {
    console.error('  --decode supports rgba8 (.ppm) or single-channel raw-u16 (.pgm)');
    process.exit(2);
  }
  await writeFile(decodeOut, Buffer.concat([Buffer.from(header), body]));
  console.log(`  Wrote ${decodeOut}`);
}
raw.close();
