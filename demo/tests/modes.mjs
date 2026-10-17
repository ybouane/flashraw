// Checks the demo under different hosting headers, and that the peeking widget loads under COEP.
//
//   node scripts/serve.mjs --port 4173 &                       # COEP require-corp (production)
//   node scripts/serve.mjs --port 4174 --coep off &            # not isolated
//   node scripts/serve.mjs --port 4175 --coep credentialless &
//   CORPUS=/path/to/corpus node tests/modes.mjs [--browsers chromium,firefox] [--widget]
//
// --widget waits ~20 s per page for the widget's first peek (introDelayMs 15 s).
// DEMO_PORTS=4183,4184,4185 uses other ports for the three servers.

import { chromium, firefox, webkit } from 'playwright';
import { join } from 'node:path';

const CORPUS = process.env.CORPUS ?? '/var/projects/flashraw/corpus';
const FILE = join(CORPUS, 'Nikon/D850/Nikon-D850-14bit-lossless-compressed.NEF');
const args = process.argv.slice(2);
const browsers = (args.includes('--browsers') ? args[args.indexOf('--browsers') + 1] : 'chromium').split(',');
const widget = args.includes('--widget');
const [P_PROD, P_OFF, P_CRED] = (process.env.DEMO_PORTS ?? '4173,4174,4175').split(',');
// [label, url, expected build]: isolated pages get the threaded build, the others SIMD
const CASES = [
  ['require-corp', `http://localhost:${P_PROD}/`, 'threads'],
  ['not isolated', `http://localhost:${P_OFF}/`, 'simd'],
  ['credentialless', `http://localhost:${P_CRED}/`, 'threads'],
  ['CPU renderer', `http://localhost:${P_PROD}/?renderer=cpu`, 'threads'],
];

let failures = 0;
for (const b of browsers) {
  const launcher = { chromium, firefox, webkit }[b];
  const executablePath = process.env[`DEMO_${b.toUpperCase()}`] || undefined;
  const browser = await launcher.launch({ executablePath });
  console.log(`\n=== ${b}`);
  for (const [label, url, binary] of CASES) {
    const page = await browser.newPage({ viewport: { width: 1280, height: 860 } });
    const errors = [];
    const widgetResponses = [];
    page.on('pageerror', (e) => errors.push(String(e)));
    page.on('console', (m) => m.type() === 'error' && errors.push(m.text()));
    // media elements cancel range requests (net::ERR_ABORTED): not a blocked resource
    page.on('requestfailed', (r) => r.url().includes('peeking') && r.failure()?.errorText !== 'net::ERR_ABORTED' && errors.push(`request failed ${r.url()} ${r.failure()?.errorText}`));
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
      binary: document.getElementById('perf-engine').dataset.binary,
    }));
    const pick = (k) => r.dd.find((x) => x.startsWith(k)) ?? '';
    let w = '';
    if (widget) {
      await page.waitForTimeout(20_000);
      w = await page.evaluate(() => `widget: ${document.querySelectorAll('ybouane-peeking').length} element(s), PeekingWidget=${typeof window.PeekingWidget}`);
      w += ` · ${widgetResponses.join(', ')}`;
    }
    // WebKit does not support credentialless: not isolated there, so the SIMD build
    const want = label === 'credentialless' && b === 'webkit' ? 'simd' : binary;
    const bad = r.error || errors.length || r.binary !== want;
    if (bad) failures++;
    console.log(`  ${bad ? 'FAIL' : 'ok  '} ${label}: isolated=${r.iso} · ${r.binary} build (expected ${want}) · ${r.status}`);
    console.log(`       ${pick('Decoder')} · ${pick('WASM binary')} · ${pick('Threads')} · ${pick('Requested')}`);
    if (label === 'CPU renderer') console.log(`       ${r.render}`);
    if (w) console.log(`       ${w}`);
    if (errors.length) console.log(`       errors: ${errors.join(' || ')}`);
    await page.close();
  }
  await browser.close();
}
console.log(failures ? `\n${failures} failure(s)` : '\nall modes passed');
process.exit(failures ? 1 : 0);
