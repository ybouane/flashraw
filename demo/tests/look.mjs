// Compares the default rendering with each file's embedded camera preview: mean brightness
// (sRGB luma 0..255), the exposure "Auto" would pick, and screenshots of both views.
//
//   npm run serve &   CORPUS=/path/to/corpus node tests/look.mjs
//
// Used to calibrate LOOK in src/pipeline.ts: the median Auto EV should be close to 0.

import { chromium } from 'playwright';
import { mkdirSync } from 'node:fs';
import { join } from 'node:path';

const URL_ = process.env.DEMO_URL ?? 'http://localhost:4173/';
const CORPUS = process.env.CORPUS ?? '/var/projects/flashraw/corpus';
const OUT = process.env.OUT ?? 'test-results';
mkdirSync(OUT, { recursive: true });
const FILES = [
  ['cr3', 'Canon/EOS R5/Canon_EOS_R5_RAW_ISO_100_nocrop_nodual.CR3'],
  ['cr3-r10', 'Canon/EOS R10/IMG_4361.CR3'],
  ['nef', 'Nikon/D850/Nikon-D850-14bit-lossless-compressed.NEF'],
  ['nef-z8', 'Nikon/Z 8/Nikon_Z8_raw_14_bit_lossless_compression.NEF'],
  ['arw', 'Sony/ILCE-7M4/ILCE-7M4_DSC06677_FullFrame-Raw-Compressed.ARW'],
  ['raf', 'Fujifilm/X-T5/DSCF0021.RAF'],
  ['orf', 'OM System/OM-1/OM_Systems_OM1_RAW_Image_20mpx_P1010025.ORF'],
  ['rw2', 'Panasonic/DC-S5M2/P1126458_mechanical.RW2'],
  ['dng-iphone', 'Apple/iPhone 12 Pro/IMG_1361.DNG'],
  ['dng-samsung', 'Samsung/Galaxy S22 Ultra/20230531_111518.dng'],
  ['dng-leica', 'Leica/Q2/L1000750.DNG'],
];

const browser = await chromium.launch({ args: ['--enable-unsafe-swiftshader'] });
const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
await page.goto(URL_);
const evs = [];
for (const [key, rel] of FILES) {
  await page.setInputFiles('#file-input', join(CORPUS, rel));
  await page.waitForFunction(() => /decoded|error/.test(document.getElementById('status').textContent), null, { timeout: 120_000 });
  await page.waitForTimeout(300);
  const before = await page.evaluate(() => ({ ...document.getElementById('histogram').dataset }));
  await page.locator('.viewer-card').screenshot({ path: join(OUT, `look-${key}-raw.png`) });
  await page.click('#src-preview');
  await page.waitForTimeout(300);
  await page.locator('.viewer-card').screenshot({ path: join(OUT, `look-${key}-preview.png`) });
  await page.click('#src-raw');
  await page.click('#auto-adjust');
  await page.waitForTimeout(300);
  const ev = Number(await page.inputValue('#adj-exposure'));
  const after = await page.evaluate(() => document.getElementById('histogram').dataset.mean);
  evs.push(ev);
  console.log(`${key.padEnd(12)} default mean ${before.mean} · preview ${before.previewMean} · auto ${ev >= 0 ? '+' : ''}${ev.toFixed(2)} EV → ${after}`);
  await page.click('#reset-adjust');
}
evs.sort((a, b) => a - b);
console.log(`median auto EV ${evs[evs.length >> 1].toFixed(2)} (range ${evs[0]} .. ${evs[evs.length - 1]})`);
await browser.close();
