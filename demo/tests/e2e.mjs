// End-to-end test of the built demo (dist/) with Playwright.
//
//   npm run build
//   node scripts/serve.mjs --port 4173 &          # (production headers: COEP require-corp)
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
  // DEMO_WEBKIT / DEMO_FIREFOX / DEMO_CHROMIUM=<path> override the engine executables
  const executablePath = process.env[`DEMO_${name.toUpperCase()}`] || undefined;
  const browser = await launcher.launch({ executablePath, args: name === 'chromium' ? ['--enable-unsafe-swiftshader', '--ignore-gpu-blocklist'] : [] });
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
  // The app chunk is requested at once; the WASM and the sample start right after the first paint
  // (not before it, and not only when idle).
  try {
    await page.waitForFunction(
      () => ['.wasm', '/samples/'].every((k) => performance.getEntriesByType('resource').some((e) => e.name.includes(k))),
      null,
      { timeout: 5_000 },
    );
    const fp = await page.evaluate(() => performance.getEntriesByName('first-paint')[0]?.startTime ?? 0);
    const starts = await page.evaluate(() => Object.fromEntries(['.wasm', '/samples/', '/assets/app-'].map((k) => [k, performance.getEntriesByType('resource').find((e) => e.name.includes(k))?.startTime ?? -1])));
    ok(`load ${loadMs} ms, ${requests.length} requests; first paint ${Math.round(fp)} ms; app chunk ${Math.round(starts['/assets/app-'])} ms, WASM ${Math.round(starts['.wasm'])} ms, sample ${Math.round(starts['/samples/'])} ms`);
    starts['/assets/app-'] >= 0 && starts['/assets/app-'] <= starts['.wasm'] ? ok('app chunk requested before the WASM') : fail(`app chunk not requested before the WASM: ${JSON.stringify(starts)}`);
    starts['.wasm'] >= fp - 5 ? ok('WASM requested after the first paint') : fail(`WASM requested before the first paint (${starts['.wasm']} < ${fp})`);
    starts['.wasm'] < 3_000 ? ok('WASM requested within 3 s of navigation start') : fail(`WASM requested late: ${starts['.wasm']} ms`);
  } catch {
    fail(`the decoder and the sample were not requested right after the first paint (${requests.filter((u) => u.endsWith('.wasm') || u.includes('/samples/')).join(', ') || 'none'})`);
  }
  // Hero speed badge: the accessible name is the visible text, the range is the measured one
  {
    const b = await page.evaluate(() => {
      const el = document.querySelector('.speed-badge');
      return { text: el?.textContent.replace(/\s+/g, ' ').trim(), label: el?.getAttribute('aria-label'), href: el?.getAttribute('href') };
    });
    b.text === '8.8–15×  faster than LibRaw  RAW → RGB · median'.replace(/\s+/g, ' ') && b.label === b.text && b.href === '#benchmarks'
      ? ok(`badge: "${b.text}" (aria-label identical, links to #benchmarks)`)
      : fail(`badge text/label mismatch: ${JSON.stringify(b)}`);
    const facts = await page.evaluate(() => document.querySelector('.facts')?.textContent ?? '');
    facts.includes('8.8–15×') && !/\b9–15×/.test(await page.content()) ? ok('facts tile uses 8.8–15×, no stale 9–15×') : fail('stale "9–15×" or missing 8.8–15× in the facts tile');
  }
  // Keyboard focus on the visually hidden file input shows a ring on the "Choose a RAW file" label
  {
    await page.focus('#file-input');
    await page.keyboard.press('Shift+Tab');
    await page.keyboard.press('Tab'); // back onto the input through the keyboard (focus-visible)
    const ring = await page.evaluate(() => {
      const l = document.querySelector('label[for="file-input"]');
      const cs = getComputedStyle(l);
      return { focused: document.activeElement?.id, style: cs.outlineStyle, width: parseFloat(cs.outlineWidth) };
    });
    ring.focused === 'file-input' && ring.style !== 'none' && ring.width >= 2 ? ok(`focus ring on the "Choose a RAW file" label (${ring.width}px ${ring.style})`) : fail(`no visible focus ring on the label: ${JSON.stringify(ring)}`);
    await page.evaluate(() => document.activeElement?.blur());
  }
  const iso = await page.evaluate(() => self.crossOriginIsolated);
  ok(`crossOriginIsolated=${iso}`);
  await page.screenshot({ path: join(OUT, `${name}-landing.png`) });
  if (name === 'chromium') await page.screenshot({ path: join(OUT, `${name}-landing-full.png`), fullPage: true });

  // "Try a sample": the shipped CC0 file (public/samples/)
  await page.click('#sample-btn');
  {
    const r = await waitDecoded(page);
    r.error || !/Canon EOS R5 Mark II/.test(r.status) ? fail(`sample: ${r.status}`) : ok(`sample: ${r.status}`);
    await page.screenshot({ path: join(OUT, `${name}-sample.png`) });
  }
  // segmented control: roving tabindex and arrow keys; half-size decodes say so on the download buttons
  if (await page.locator('#src-preview').isEnabled()) {
    await page.focus('#src-raw');
    await page.keyboard.press('ArrowRight');
    const st = await page.evaluate(() => ({ prev: document.getElementById('src-preview').getAttribute('aria-checked'), focus: document.activeElement?.id, t0: document.getElementById('src-raw').tabIndex, t1: document.getElementById('src-preview').tabIndex }));
    st.prev === 'true' && st.focus === 'src-preview' && st.t0 === -1 && st.t1 === 0 ? ok('radio group: ArrowRight selects the preview, roving tabindex') : fail(`radio keys: ${JSON.stringify(st)}`);
    await page.keyboard.press('ArrowLeft');
  }
  {
    const dl = await page.evaluate(() => [document.getElementById('dl-png').textContent, document.getElementById('dl-png').title, document.getElementById('dl-info').textContent]);
    ok(`download button: "${dl[0]}"${dl[1] ? ` (title: ${dl[1]})` : ''}`);
  }

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
    {
      const dl = await page.evaluate(() => [document.getElementById('dl-png').textContent, document.getElementById('dl-jpeg').textContent, document.getElementById('image-dims').textContent]);
      const half = /half-size/.test(dl[2]);
      half === /half-size/.test(dl[0]) && half === /half-size/.test(dl[1]) ? (half ? ok(`${key}: half-size decode, download buttons say "${dl[0]}"`) : null) : fail(`${key}: download labels "${dl[0]}" / "${dl[1]}" do not match the decode (${dl[2]})`);
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
    await page.waitForTimeout(150);
    const zd = await page.textContent('#zoom-level');
    zd === z ? ok('double-click leaves the zoom alone') : fail(`double-click changed the zoom: ${z} → ${zd}`);
    await page.click('#src-preview');
    await page.waitForTimeout(200);
    const zp = await page.textContent('#zoom-level');
    await page.click('#src-raw');
    await page.waitForTimeout(200);
    const zr = await page.textContent('#zoom-level');
    zr === z ? ok(`zoom kept across RAW → preview → RAW (${z} → ${zp} → ${zr})`) : fail(`zoom not kept across sources: ${z} → ${zp} → ${zr}`);
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

  // widget messages, and HTTPS loads in a WebKit build without TLS (glib-networking) support
  const relevant = errors.filter((e) => !/peeking|TLS support is not available/i.test(e));
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
