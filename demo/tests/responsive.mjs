// No horizontal overflow at phone and tablet widths, before and after a decode, with every
// collapsible part open (decoder options, metadata JSON, chart tables) and every install tab.
//
//   node scripts/serve.mjs --port 4173 &
//   DEMO_URL=http://localhost:4173/ node tests/responsive.mjs [--browsers chromium,firefox,webkit] [--shots]
//
// Asserts document.documentElement.scrollWidth <= innerWidth == the device width in each state, with the page's
// `overflow-x: hidden` safety net switched off, so a cause cannot hide behind it. Lists the
// elements sticking out when it fails. --shots saves full-page screenshots to $OUT.

import { chromium, firefox, webkit } from 'playwright';
import { mkdirSync } from 'node:fs';
import { join } from 'node:path';

const URL_ = process.env.DEMO_URL ?? 'http://localhost:4173/';
const OUT = process.env.OUT ?? 'test-results';
const args = process.argv.slice(2);
const browsers = (args.includes('--browsers') ? args[args.indexOf('--browsers') + 1] : 'chromium').split(',');
const shots = args.includes('--shots');
const WIDTHS = (process.env.WIDTHS ?? '320,375,390,430,768').split(',').map(Number);
mkdirSync(OUT, { recursive: true });

let failures = 0;

/** scrollWidth - innerWidth with the overflow-x guard disabled, plus the offending elements. */
async function overflow(page, width) {
  return page.evaluate((width) => {
    const unguarded = window.innerWidth;
    const guard = document.createElement('style');
    guard.textContent = 'html, body { overflow-x: visible !important; }';
    document.head.append(guard);
    // On mobile, content wider than the screen widens the layout viewport (innerWidth grows):
    // compare with the device width as well.
    const over = Math.max(document.documentElement.scrollWidth, window.innerWidth) - width;
    const culprits = [];
    if (over > 0) {
      const clipped = (el) => {
        for (let p = el.parentElement; p; p = p.parentElement) {
          const o = getComputedStyle(p).overflowX;
          if (o === 'auto' || o === 'scroll' || o === 'hidden' || o === 'clip') return p !== document.body && p !== document.documentElement;
        }
        return false;
      };
      for (const el of document.querySelectorAll('body *')) {
        const r = el.getBoundingClientRect();
        if (r.width && r.right > width + 0.5 && !clipped(el)) {
          culprits.push(`${el.tagName.toLowerCase()}${el.id ? '#' + el.id : ''}${el.className && typeof el.className === 'string' ? '.' + el.className.trim().split(/\s+/).join('.') : ''} → ${Math.round(r.right)}px ${getComputedStyle(el).position}`);
        }
      }
    }
    guard.remove();
    return { over, culprits: [`with the guard: innerWidth ${unguarded}; without: scrollWidth ${document.documentElement.scrollWidth}, innerWidth ${window.innerWidth}`, ...culprits.slice(0, 8)] };
  }, width);
}

async function check(page, label) {
  const { over, culprits } = await overflow(page, page.viewportSize().width);
  if (over > 0) {
    failures++;
    console.log(`  FAIL ${label}: ${over}px horizontal overflow\n         ${culprits.join('\n         ')}`);
  } else console.log(`  ok   ${label}`);
}

async function openAll(page) {
  await page.evaluate(() => document.querySelectorAll('details').forEach((d) => (d.open = true)));
}

for (const b of browsers) {
  const launcher = { chromium, firefox, webkit }[b];
  const executablePath = process.env[`DEMO_${b.toUpperCase()}`] || undefined;
  const browser = await launcher.launch({ executablePath });
  console.log(`\n=== ${b}`);
  for (const width of WIDTHS) {
    const ctx = await browser.newContext({ viewport: { width, height: 800 }, deviceScaleFactor: 1, hasTouch: width < 700, isMobile: b === 'chromium' && width < 700 });
    const page = await ctx.newPage();
    await page.goto(URL_, { waitUntil: 'load' });
    await check(page, `${width}px landing`);
    await page.locator('#benchmarks').scrollIntoViewIfNeeded();
    await page.waitForSelector('#charts[data-state="ready"]');
    await openAll(page);
    for (const tab of ['tab-npm', 'tab-rust', 'tab-python', 'tab-c']) {
      await page.click(`#${tab}`);
      await check(page, `${width}px charts + tables + ${tab}`);
    }
    // all cameras / format pairs, both metrics
    for (const btn of await page.locator('.chart-more button:visible').all()) await btn.click();
    for (const seg of await page.locator('.chart-controls [role="radio"]').all()) {
      await seg.click();
      await check(page, `${width}px chart "${await seg.textContent()}"`);
    }
    if (shots) await page.screenshot({ path: join(OUT, `responsive-${b}-${width}-landing.png`), fullPage: true });

    await page.locator('#sample-btn').scrollIntoViewIfNeeded();
    await page.click('#sample-btn');
    await page.waitForFunction(() => /decoded/.test(document.getElementById('status').textContent) || document.getElementById('status').classList.contains('error'), null, { timeout: 180_000 });
    const status = await page.textContent('#status');
    if (await page.evaluate(() => document.getElementById('status').classList.contains('error'))) {
      failures++;
      console.log(`  FAIL ${width}px decode: ${status}`);
    }
    await openAll(page);
    await check(page, `${width}px after decode (${status.slice(0, 60)}…)`);
    await page.click('#src-preview').catch(() => {});
    await check(page, `${width}px embedded preview`);
    if (shots) await page.screenshot({ path: join(OUT, `responsive-${b}-${width}-decoded.png`), fullPage: true });
    await ctx.close();
  }
  await browser.close();
}
console.log(failures ? `\n${failures} failure(s)` : '\nno horizontal overflow');
process.exit(failures ? 1 : 0);
