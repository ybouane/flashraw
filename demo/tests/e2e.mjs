// End-to-end test of the built demo (dist/) with Playwright.
//
//   npm run build
//   node scripts/serve.mjs --port 4173 &          # (also: --coep off, --coep credentialless)
//   DEMO_URL=http://localhost:4173/ CORPUS=/path/to/corpus node tests/e2e.mjs [--browsers chromium,firefox,webkit] [--quick]
//
// Screenshots go to $OUT (default: test-results/). Exits non-zero on any failure.

import { chromium, firefox, webkit, devices } from 'playwright';
import { mkdirSync, statSync } from 'node:fs';
import { join } from 'node:path';

const URL_ = process.env.DEMO_URL ?? 'http://localhost:4173/';
const CORPUS = process.env.CORPUS ?? '/var/projects/flashraw/corpus';
const OUT = process.env.OUT ?? 'test-results';
const args = process.argv.slice(2);
const browsersArg = args.includes('--browsers') ? args[args.indexOf('--browsers') + 1] : 'chromium';
const quick = args.includes('--quick');
mkdirSync(OUT, { recursive: true });

const FILES = [
  ['cr3', 'Canon/EOS R5/Canon_EOS_R5_RAW_ISO_100_nocrop_nodual.CR3'],
  ['nef', 'Nikon/D850/Nikon-D850-14bit-lossless-compressed.NEF'],
  ['arw', 'Sony/ILCE-7M4/ILCE-7M4_DSC06677_FullFrame-Raw-Compressed.ARW'],
  ['raf', 'Fujifilm/X-T5/DSCF0021.RAF'],
  ['dng-leica', 'Leica/Q2/L1000750.DNG'],
  ['dng-iphone-portrait', 'Apple/iPhone 12 Pro/IMG_1361.DNG'],
  ['dng-jxl', 'Samsung/Galaxy S22 Ultra/20230531_111518.dng'],
  ['orf', 'OM System/OM-1/OM_Systems_OM1_RAW_Image_20mpx_P1010025.ORF'],
  ['rw2', 'Panasonic/DC-S5M2/P1126458_mechanical.RW2'],
  ['cr3-portrait', 'Canon/EOS R10/IMG_4361.CR3'],
  ['raf-100mp', 'Fujifilm/GFX100 II/_DSF0265.RAF'],
];

let failures = 0;
const fail = (msg) => {
  failures++;
  console.log(`  FAIL ${msg}`);
};
const ok = (msg) => console.log(`  ok   ${msg}`);

async function waitDecoded(page, timeout = 120_000) {
  await page.waitForFunction(
    () => {
      const s = document.getElementById('status');
      return s && (/decoded/.test(s.textContent) || s.classList.contains('error'));
    },
    null,
    { timeout },
  );
  return page.evaluate(() => ({
    status: document.getElementById('status').textContent,
    error: document.getElementById('status').classList.contains('error'),
  }));
}

async function histogramInk(page) {
  return page.evaluate(() => {
    const c = document.getElementById('histogram');
    const d = c.getContext('2d').getImageData(0, 0, c.width, c.height).data;
    let n = 0;
    for (let i = 3; i < d.length; i += 4) if (d[i] > 0) n++;
    return n / (c.width * c.height);
  });
}

async function run(name, launcher) {
  console.log(`\n=== ${name}`);
  const browser = await launcher.launch({ args: name === 'chromium' ? ['--enable-unsafe-swiftshader', '--ignore-gpu-blocklist'] : [] });
  const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 }, deviceScaleFactor: 1, acceptDownloads: true });
  const page = await ctx.newPage();
  const errors = [];
  page.on('console', (m) => m.type() === 'error' && errors.push(m.text()));
  page.on('pageerror', (e) => errors.push(String(e)));
  const requests = [];
  page.on('request', (r) => requests.push(r.url()));

  const t = Date.now();
  await page.goto(URL_, { waitUntil: 'load' });
  const loadMs = Date.now() - t;
  const wasmEarly = requests.filter((u) => u.endsWith('.wasm'));
  wasmEarly.length ? fail(`WASM loaded before any interaction: ${wasmEarly}`) : ok(`no WASM before interaction (load ${loadMs} ms, ${requests.length} requests)`);
  const iso = await page.evaluate(() => self.crossOriginIsolated);
  ok(`crossOriginIsolated=${iso}`);
  await page.screenshot({ path: join(OUT, `${name}-landing.png`) });
  if (name === 'chromium') await page.screenshot({ path: join(OUT, `${name}-landing-full.png`), fullPage: true });

  const files = quick ? FILES.slice(0, 2) : name === 'chromium' ? FILES : FILES.filter(([k]) => ['cr3', 'nef', 'dng-iphone-portrait'].includes(k));
  for (const [key, rel] of files) {
    const path = join(CORPUS, rel);
    try {
      statSync(path);
    } catch {
      console.log(`  skip ${key}: ${rel} not in corpus`);
      continue;
    }
    const t0 = Date.now();
    await page.setInputFiles('#file-input', path);
    const r = await waitDecoded(page, key.includes('100mp') ? 240_000 : 120_000);
    const wall = Date.now() - t0;
    if (r.error) {
      fail(`${key}: ${r.status}`);
      await page.screenshot({ path: join(OUT, `${name}-${key}-error.png`) });
      continue;
    }
    const info = await page.evaluate(() => ({
      camera: document.querySelector('.meta-hero')?.textContent,
      tiles: [...document.querySelectorAll('.tile')].map((t) => `${t.querySelector('.tile-label').textContent}=${t.querySelector('.tile-value').textContent}`).join(' | '),
      backend: [...document.querySelectorAll('#perf-backend dd')].slice(0, 4).map((d) => d.textContent).join(' | '),
      dims: document.getElementById('image-dims').textContent,
      cards: document.querySelectorAll('.meta-card').length,
      previews: document.querySelectorAll('.previews-card tbody tr').length,
      clip: document.getElementById('clip-info').textContent,
    }));
    ok(`${key}: ${info.camera} · ${info.dims} · ${wall} ms wall · ${info.tiles}`);
    console.log(`       backend: ${info.backend} · cards ${info.cards} · previews ${info.previews} · ${info.clip}`);
    const ink = await histogramInk(page);
    ink > 0.02 ? ok(`histogram drawn (${(ink * 100).toFixed(0)} % ink)`) : fail(`${key}: histogram empty`);
    await page.locator('#workspace').scrollIntoViewIfNeeded();
    await page.locator('.viewer-card').screenshot({ path: join(OUT, `${name}-${key}-viewer.png`) });
  }

  if (name === 'chromium' && !quick) {
    // interaction checks on the CR3 (has three previews)
    await page.setInputFiles('#file-input', join(CORPUS, FILES[0][1]));
    await waitDecoded(page);
    const before = await page.textContent('#clip-info');
    await page.locator('#adj-exposure').fill('2');
    await page.waitForTimeout(250);
    const after = await page.textContent('#clip-info');
    before !== after ? ok(`exposure slider changes the histogram (${before} → ${after})`) : fail('exposure slider had no effect on histogram');
    for (const [id, v] of [['#adj-contrast', '30'], ['#adj-highlights', '-60'], ['#adj-shadows', '40'], ['#adj-temperature', '25'], ['#adj-vibrance', '30']]) await page.locator(id).fill(v);
    await page.locator('#adj-exposure').fill('0.6');
    await page.waitForTimeout(300);
    await page.locator('.viewer-card').screenshot({ path: join(OUT, `${name}-adjusted.png`) });
    await page.locator('.ws-side').screenshot({ path: join(OUT, `${name}-sidebar.png`) });

    await page.click('#zoom-1');
    (await page.textContent('#zoom-level')) === '100%' ? ok('1:1 zoom') : fail(`1:1 zoom shows ${await page.textContent('#zoom-level')}`);
    const box = await page.locator('#stage').boundingBox();
    await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
    await page.mouse.down();
    await page.mouse.move(box.x + box.width / 2 - 200, box.y + box.height / 2 - 120, { steps: 5 });
    await page.mouse.up();
    await page.mouse.wheel(0, -400);
    await page.waitForTimeout(200);
    const z = await page.textContent('#zoom-level');
    z !== '100%' ? ok(`wheel zoom → ${z}`) : fail('wheel zoom had no effect');
    await page.locator('.viewer-card').screenshot({ path: join(OUT, `${name}-zoomed.png`) });
    await page.dblclick('#stage');
    await page.click('#zoom-fit');

    await page.click('#src-preview');
    await page.waitForTimeout(200);
    const pd = await page.textContent('#image-dims');
    /Embedded/.test(pd) ? ok(`preview source: ${pd}`) : fail(`preview toggle: ${pd}`);
    await page.locator('.viewer-card').screenshot({ path: join(OUT, `${name}-preview.png`) });
    await page.click('#src-raw');

    for (const [btn, ext] of [['#dl-jpeg', 'jpg'], ['#dl-preview', 'jpg'], ['#dl-png', 'png']]) {
      const [dl] = await Promise.all([page.waitForEvent('download', { timeout: 180_000 }), page.click(btn)]);
      const p = join(OUT, `download-${btn.slice(1)}.${ext}`);
      await dl.saveAs(p);
      const size = statSync(p).size;
      size > 10_000 ? ok(`${btn} → ${dl.suggestedFilename()} ${(size / 1e6).toFixed(1)} MB (${await page.textContent('#dl-info')})`) : fail(`${btn} download too small (${size})`);
    }
    // keyboard: focus stage, + and 0
    await page.focus('#stage');
    await page.keyboard.press('+');
    await page.keyboard.press('0');
    ok('keyboard zoom keys handled');
    await page.locator('#perf-title').scrollIntoViewIfNeeded();
    await page.locator('.perf').screenshot({ path: join(OUT, `${name}-perf.png`) });
    await page.locator('.meta').screenshot({ path: join(OUT, `${name}-metadata.png`) });
    await page.locator('#benchmarks').scrollIntoViewIfNeeded();
    await page.waitForSelector('#charts[data-state="ready"]');
    await page.locator('#benchmarks').screenshot({ path: join(OUT, `${name}-benchmarks.png`) });
    await page.screenshot({ path: join(OUT, `${name}-demo-full.png`), fullPage: true });
  }

  const relevant = errors.filter((e) => !/peeking/i.test(e));
  relevant.length ? fail(`console errors: ${relevant.join(' || ')}`) : ok(`no console errors${errors.length ? ` (ignored ${errors.length} widget messages)` : ''}`);
  await ctx.close();

  if (name === 'chromium') {
    // mobile
    const m = await browser.newContext({ ...devices['Pixel 7'] });
    const mp = await m.newPage();
    const merr = [];
    mp.on('pageerror', (e) => merr.push(String(e)));
    await mp.goto(URL_);
    await mp.screenshot({ path: join(OUT, 'mobile-landing.png') });
    await mp.setInputFiles('#file-input', join(CORPUS, FILES[2][1]));
    const r = await waitDecoded(mp);
    r.error ? fail(`mobile: ${r.status}`) : ok(`mobile decode: ${r.status}`);
    await mp.locator('#workspace').scrollIntoViewIfNeeded();
    await mp.screenshot({ path: join(OUT, 'mobile-demo.png') });
    const overflow = await mp.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
    overflow <= 0 ? ok('mobile: no horizontal overflow') : fail(`mobile: horizontal overflow ${overflow}px`);
    await mp.screenshot({ path: join(OUT, 'mobile-full.png'), fullPage: true });
    merr.length ? fail(`mobile errors: ${merr}`) : ok('mobile: no page errors');
    await m.close();
  }
  await browser.close();
}

const launchers = { chromium, firefox, webkit };
for (const b of browsersArg.split(',')) await run(b, launchers[b]);
console.log(failures ? `\n${failures} failure(s)` : '\nall checks passed');
process.exit(failures ? 1 : 0);
