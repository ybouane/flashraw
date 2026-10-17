// Checks the demo under different hosting headers and engine choices, and that the peeking
// widget still loads under COEP.
//
//   node scripts/serve.mjs --port 4173 &                       # COEP credentialless (production)
//   node scripts/serve.mjs --port 4174 --coep off &            # not isolated
//   node scripts/serve.mjs --port 4175 --coep require-corp &
//   CORPUS=/path/to/corpus node tests/modes.mjs [--browsers chromium,firefox] [--widget]
//
// --widget waits ~20 s per page for the widget's first peek (introDelayMs 15 s).

import { chromium, firefox, webkit } from 'playwright';
import { join } from 'node:path';

const CORPUS = process.env.CORPUS ?? '/var/projects/flashraw/corpus';
const FILE = join(CORPUS, 'Nikon/D850/Nikon-D850-14bit-lossless-compressed.NEF');
const args = process.argv.slice(2);
const browsers = (args.includes('--browsers') ? args[args.indexOf('--browsers') + 1] : 'chromium').split(',');
const widget = args.includes('--widget');
const CASES = [
  ['credentialless', 'http://localhost:4173/'],
  ['not isolated', 'http://localhost:4174/'],
  ['require-corp', 'http://localhost:4175/'],
  ['CPU renderer', 'http://localhost:4173/?renderer=cpu'],
  ['WebGPU build', 'http://localhost:4173/?engine=gpu'],
  ['threaded build', 'http://localhost:4173/?engine=threads'],
  ['single-threaded', 'http://localhost:4173/?engine=single'],
  ['scalar build', 'http://localhost:4173/?engine=scalar'],
];

let failures = 0;
for (const b of browsers) {
  const launcher = { chromium, firefox, webkit }[b];
  const executablePath = process.env[`DEMO_${b.toUpperCase()}`] || undefined;
  const browser = await launcher.launch({ executablePath });
  console.log(`\n=== ${b}`);
  for (const [label, url] of CASES) {
    const page = await browser.newPage({ viewport: { width: 1280, height: 860 } });
    const errors = [];
    const widgetResponses = [];
    page.on('pageerror', (e) => errors.push(String(e)));
    page.on('console', (m) => m.type() === 'error' && errors.push(m.text()));
    page.on('requestfailed', (r) => r.url().includes('peeking') && errors.push(`request failed ${r.url()} ${r.failure()?.errorText}`));
    page.on('response', (r) => r.url().includes('peeking.ybouane.com') && widgetResponses.push(`${r.status()} ${r.url().split('/').pop()}`));
    await page.goto(url);
    await page.setInputFiles('#file-input', FILE);
    await page.waitForFunction(() => /decoded|error/.test(document.getElementById('status').textContent) || document.getElementById('status').classList.contains('error'), null, { timeout: 180_000 });
    const r = await page.evaluate(() => ({
      status: document.getElementById('status').textContent,
      error: document.getElementById('status').classList.contains('error'),
      iso: self.crossOriginIsolated,
      dd: [...document.querySelectorAll('#perf-backend dt')].map((dt) => `${dt.textContent}: ${dt.nextElementSibling.textContent}`),
      render: document.getElementById('render-info').textContent,
    }));
    const pick = (k) => r.dd.find((x) => x.startsWith(k)) ?? '';
    let w = '';
    if (widget) {
      await page.waitForTimeout(20_000);
      w = await page.evaluate(() => `widget: ${document.querySelectorAll('ybouane-peeking').length} element(s), PeekingWidget=${typeof window.PeekingWidget}`);
      w += ` · ${widgetResponses.join(', ')}`;
    }
    // require-corp blocks the widget's analytics beacon (no CORP header on /t): expected, and the
    // reason production uses credentialless
    const unexpected = label === 'require-corp' ? errors.filter((e) => !/peeking\.ybouane\.com\/t|Cross-Origin-Resource-Policy|NotSameOriginAfterDefaultedToSameOriginByCoep/.test(e)) : errors;
    if (unexpected.length !== errors.length) console.log('       (expected under require-corp: the widget beacon /t is blocked)');
    const bad = r.error || unexpected.length;
    if (bad) failures++;
    console.log(`  ${bad ? 'FAIL' : 'ok  '} ${label}: isolated=${r.iso} · ${r.status}`);
    console.log(`       ${pick('Decoder')} · ${pick('WASM binary')} · ${pick('Threads')} · ${pick('GPU')} · ${pick('Requested')}`);
    if (label === 'CPU renderer') console.log(`       ${r.render}`);
    if (w) console.log(`       ${w}`);
    if (errors.length) console.log(`       errors: ${errors.join(' || ')}`);
    await page.close();
  }
  await browser.close();
}
// Default engine with WebGPU (Chromium with a software adapter): the WebGPU build with the GPU
// backend; then switching builds re-opens the file without a reload.
if (browsers.includes('chromium')) {
  console.log('\n=== chromium --enable-unsafe-webgpu: default engine and switching');
  const browser = await chromium.launch({ executablePath: process.env.DEMO_CHROMIUM || undefined, args: ['--enable-unsafe-webgpu', '--enable-unsafe-swiftshader'] });
  const page = await browser.newPage({ viewport: { width: 1280, height: 860 } });
  const errors = [];
  page.on('pageerror', (e) => errors.push(String(e)));
  const done = () => page.waitForFunction(() => /decoded/.test(document.getElementById('status').textContent) || document.getElementById('status').classList.contains('error'), null, { timeout: 180_000 });
  const state = () => page.evaluate(() => ({ status: document.getElementById('status').textContent, error: document.getElementById('status').classList.contains('error'), ran: document.getElementById('perf-engine').dataset, marked: window.__mark === 1 }));
  const expect = (cond, msg) => {
    if (!cond) failures++;
    console.log(`  ${cond ? 'ok  ' : 'FAIL'} ${msg}`);
  };
  await page.goto('http://localhost:4173/');
  await page.setInputFiles('#file-input', FILE);
  await done();
  let r = await state();
  expect(!r.error && r.ran.kind === 'gpu' && r.ran.binary === 'gpu', `default with WebGPU: ${r.status} (${r.ran.binary}, ${r.ran.kind})`);
  for (const [engine, binary, kind] of [['threads', 'threads', 'cpu'], ['single', 'simd', 'cpu'], ['gpu', 'gpu', 'gpu'], ['threads', 'threads', 'cpu']]) {
    await page.evaluate(() => {
      window.__mark = 1;
      document.getElementById('status').textContent = '';
      document.querySelector('.decode-opts').open = true;
    });
    await page.selectOption('#opt-engine', engine);
    await done();
    r = await state();
    expect(!r.error && r.marked && r.ran.binary === binary && r.ran.kind === kind, `switch to ${engine} without reload: ${r.status} (${r.ran.binary}, ${r.ran.kind}, same document: ${r.marked})`);
  }
  expect(!errors.length, `no page errors${errors.length ? `: ${errors.join(' || ')}` : ''}`);
  await browser.close();
}
console.log(failures ? `\n${failures} failure(s)` : '\nall modes passed');
process.exit(failures ? 1 : 0);
