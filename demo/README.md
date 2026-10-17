# FlashRaw web demo

The site at <https://flashraw.ybouane.com>: the project page plus a live demo that decodes a
RAW file in the browser with the `flashraw` npm package (this repository's `npm/`).

The page opens on the demo: a headline and a drop zone with "Choose a RAW file" and "Try a
sample". What the demo does with a file (nothing is uploaded):

1. `FlashRaw.open(file)`: metadata (camera, lens, exposure, size, CFA, levels, white
   balance, colour matrices, orientation, compression, DNG fields, previews, vendor extras).
2. `raw.preview()`: the largest embedded preview, decoded by the browser and shown at once.
3. `raw.decode({ output: 'linear-rgb-f16', zeroCopy: true })`: the RAW decode, once, to
   linear light. The half floats go straight from WASM memory into an `RGB16F` WebGL2
   texture (plus a half-float mip pyramid for zoomed-out views).
4. Adjustments (exposure, contrast, highlights, shadows, temperature, tint, vibrance,
   saturation, tone curve), sRGB encoding and the histogram run in a fragment shader: a slider
   move is one draw call. All sliders at 0 mean the default "Film" look, calibrated against
   the cameras' embedded JPEGs (`tests/look.mjs`): the file's DNG BaselineExposure (or a
   camera default) plus a base exposure, a mild log-space S-curve, a saturation lift, a soft
   highlight shoulder and neutralised clipped highlights (`LOOK` in `src/pipeline.ts`). The
   tone-curve selector's "Linear" gives FlashRaw's neutral output; "Auto" sets Exposure so the
   mean brightness matches the embedded preview. Without WebGL2 the same pipeline runs in JavaScript
   (`src/renderer-cpu.ts`; force it with `?renderer=cpu`).
5. Downloads: the adjusted image at full resolution as PNG or JPEG (rendered in 2048 px tiles),
   and every embedded preview as stored.
6. Timings: metadata parse, preview extraction and browser decode, the decode with FlashRaw's
   per-stage times (entropy decode, unpack, develop), the copy and texture upload, MP/s, and
   which WASM binary, SIMD level and thread count ran ("Ran on").

Engine: the npm package's default selection: the threaded build on cross-origin isolated pages,
otherwise single-threaded SIMD (then the scalar fallback); the reason is shown in the performance
panel. The WebGPU build is left out of the site (`dropWebGpuBuild` in `vite.config.ts`): in the
browser it cannot also use threads, so its entropy decoding runs on one core and full decodes
measured 2-11x slower than the threaded build on an Apple M1 Ultra. Adjustments render on the GPU
(WebGL2) with every build.

Loading: the landing page downloads ~210 KiB (HTML, CSS, 2 KiB of JS, three font files, logo)
before the `load` event. Then, when the main thread is idle (`requestIdleCallback`), the decoder
chunk, its JS glue and one `.wasm` (compiled while it streams) are preloaded and initialised, so
the first decode starts at once; in a second idle slot the sample file is fetched into memory at
low priority, unless `navigator.connection` reports `saveData` or a 2g/3g connection. Hovering or
focusing the drop zone, dragging a file over the page or picking a file starts the same preload
earlier. The benchmark charts are a separate chunk loaded when their section scrolls near.

## Layout

```
demo/
  index.html              page markup, SEO meta, JSON-LD, peeking widget
  src/main.ts             tiny entry: drop zone, tabs, copy buttons, lazy imports
  src/app.ts              the demo controller (open, preview, decode, UI, downloads)
  src/metadata.ts         metadata cards
  src/pipeline.ts         adjustment maths (GLSL + identical JS), orientation helpers
  src/renderer-gl.ts      WebGL2 renderer      src/renderer-cpu.ts   CPU fallback
  src/viewport.ts         zoom / pan / pinch / keyboard
  src/charts.ts           benchmark charts (SVG, linear speedup bars)  src/charts-schema.ts  their schema
  src/charts-data.sample.json   sample chart data (from docs/benchmarks.md)
  src/fonts/              newsreader-core.woff2 (preloaded subset, scripts/subset-fonts.sh), OFL.txt
  public/                 robots.txt, sitemap.xml, site.webmanifest, _headers
  public/showcase/        LightFlow and ImageFlow logos ("Used in production by")
  public/samples/         the "Try a sample" file (see below)
  vercel.json             Vercel config (headers)
  scripts/serve.mjs       static server for dist/ with the production headers
  scripts/make-sample-chart-data.mjs   regenerates the sample chart data
  tests/e2e.mjs           Playwright end-to-end test     tests/modes.mjs   hosting/engine modes
  tests/responsive.mjs    no horizontal overflow at phone/tablet widths, every state
```

Brand assets are not copied into this directory: the build reads them from `../branding/`
(`logo.svg`, `logo-dark.svg`, `logo-mark.svg`, `favicon.svg`, `favicon.ico`, `favicon-32.png`,
`apple-touch-icon.png`, `icon-192.png`, `icon-512.png`, `icon-512-maskable.png`,
`og-banner.png`) and emits them at the site root; the dev server serves them from there.
Fonts (Newsreader and Newsreader Italic for text, JetBrains Mono for labels, numbers and code;
SIL OFL 1.1) are self-hosted from `@fontsource-variable` packages, latin subset only. The roman
face is split in two: `src/fonts/newsreader-core.woff2`, the characters the page itself uses,
is preloaded so the headline does not shift when the font arrives; the full latin file covers
everything else (e.g. accented camera and lens names in the metadata). Regenerate the subset
with `sh scripts/subset-fonts.sh` (needs `pip install fonttools brotli`) after a font update.
The look follows `../branding/README.md`: ink on paper, greys and hairlines, square corners;
red, green and blue appear only where they mean colour channels (histogram, CFA, white balance).

## Sample file

`public/samples/canon-eos-r5-mark-ii.cr3` (7.6 MB, Canon EOS R5 Mark II, APS-C crop, C-RAW) is
`Canon/Canon EOS R5m2/APS-C_CRAW.CR3` from <https://raw.pixls.us> (CC0, public domain):
<https://raw.pixls.us/data/Canon/Canon%20EOS%20R5m2/APS-C_CRAW.CR3>. It is prefetched when the
page is idle (see above), or fetched when the visitor clicks "Try a sample".

## Develop and build

```sh
cd npm && npm ci && npm run build    # the WASM package (needs cargo; see npm/README.md)
cd ../demo && npm ci                 # depends on file:../npm, so it uses that fresh dist/
npm run dev                          # http://localhost:5173 (cross-origin isolated)
npm run build                        # → dist/ (static, deployable)
npm run serve                        # serve dist/ with the production headers on :4173
```

`DEMO_COEP=off npm run dev` serves the page without cross-origin isolation (single-threaded
path); `DEMO_COEP=credentialless` uses the lighter policy. `?renderer=cpu` forces the CPU
display renderer.

## Deploy

`dist/` is a static site; deploy it anywhere. It needs two things from the host:

- `.wasm` served as `application/wasm` (all common hosts do), and compression (brotli/gzip)
  for `.wasm`, `.js`, `.css`, `.html`: the decoder is ~620 KiB brotli, ~900 KiB gzip, 2.6 MiB raw.
- The cross-origin isolation headers below (optional but recommended).

Only one `.wasm` is ever downloaded per visit (plus the JPEG XL variant when a JPEG XL DNG is
decoded); the others in `dist/assets/` are alternatives picked at run time.

### Hosting headers

The multithreaded WASM build needs `SharedArrayBuffer`, which needs a cross-origin isolated
page:

```
Cross-Origin-Opener-Policy: same-origin
Cross-Origin-Embedder-Policy: require-corp
```

Without them the page still works: FlashRaw falls back to the single-threaded SIMD build (the
"Performance" panel says why), decodes are slower and run on the main thread.

**Why `require-corp`.** It is the policy every current browser supports, Safari included (since
15.2), so the threaded build runs everywhere; `credentialless` isolates the page in Chromium and
Firefox only. In exchange every cross-origin response must carry CORS or
`Cross-Origin-Resource-Policy`. The page's only cross-origin content is the peeking widget
(`https://peeking.ybouane.com`): its script is loaded with `crossorigin="anonymous"` and served
with `Access-Control-Allow-Origin: *`, and its video, images and analytics beacon (`/t`) send
`Cross-Origin-Resource-Policy: cross-origin` (tested with `tests/modes.mjs --widget`). Anything
cross-origin added later (embeds, analytics, hosted fonts) needs the same, or the browser blocks
it.

Ready-made configs:

- **Netlify, Cloudflare Pages**: `public/_headers` (copied to `dist/_headers`). Build command
  `npm run build`, output directory `dist` (base directory `demo`; the build needs `../npm/dist`
  and `../branding`, so build the npm package first or deploy a prebuilt `dist/`).
- **Vercel**: `vercel.json` (project root `demo`).
- **nginx**:

  ```nginx
  server {
    server_name flashraw.ybouane.com;
    root /var/www/flashraw/dist;
    include mime.types;              # application/wasm for .wasm
    types { application/manifest+json webmanifest; }
    gzip on;
    gzip_types text/css application/javascript application/wasm image/svg+xml application/json;
    # brotli on; brotli_types ... (with ngx_brotli)

    add_header Cross-Origin-Opener-Policy "same-origin" always;
    add_header Cross-Origin-Embedder-Policy "require-corp" always;
    add_header X-Content-Type-Options "nosniff" always;
    add_header Referrer-Policy "strict-origin-when-cross-origin" always;

    location /assets/ {
      add_header Cross-Origin-Opener-Policy "same-origin" always;
      add_header Cross-Origin-Embedder-Policy "require-corp" always;
      add_header X-Content-Type-Options "nosniff" always;
      add_header Cache-Control "public, max-age=31536000, immutable";
    }
  }
  ```

  (nginx drops inherited `add_header`s inside a `location` that has its own, hence the
  repetition.)
- **Apache**: `Header always set Cross-Origin-Opener-Policy "same-origin"` and
  `Header always set Cross-Origin-Embedder-Policy "require-corp"` (mod_headers);
  `AddType application/wasm .wasm`.
- **GitHub Pages** cannot set headers: the site works there single-threaded.

## Chart data

The benchmark charts read `virtual:charts-data`, which resolves at build time to
`../docs/charts/data.json` when that file exists and to `src/charts-data.sample.json`
otherwise (with a build warning). Dropping the real file in place needs no code change; it must
follow the schema in `src/charts-schema.ts`:

```jsonc
{
  "version": 1,
  "sample": false,                       // true only for the generated sample
  "source": "docs/benchmarks.md",
  "libraries": [{ "id": "flashraw", "name": "FlashRaw" }, { "id": "libraw", "name": "LibRaw" }, { "id": "rawspeed", "name": "RawSpeed" }],
  "machines": [{ "id": "linux", "name": "Linux x86-64", "cpu": "AMD Ryzen 7 9700X, 16 threads", "icon": "🐧" }, ...],
  "headline": [                          // speedups = other library time / FlashRaw time
    { "metric": "decode" | "rgb", "vs": "libraw", "threads": "all" | "1", "set": "popular" | "all",
      "machine": "linux", "geomean": 6.14, "median": 8.26, "wins": 57, "files": 59 }, ...],
  "cameras": [                           // wall times in ms, null = cannot decode
    { "machine": "linux", "camera": "Canon EOS R5 (RAW)", "format": "CR3", "codec": "canon-crx", "mp": 46.7,
      "decode": { "flashraw": 56.6, "libraw": 58.2, "rawspeed": null },
      "rgb": { "flashraw": 105.3, "libraw": 482.6 }, "gpu": 97.3 }, ...],
  "formats": [                           // geometric-mean speedups per format/codec
    { "format": "ARW", "codec": "sony-arw2", "files": 9,
      "speedup": { "linux": { "decodeVsLibraw": 10.2, "decodeVsRawspeed": 1.76, "rgbVsLibraw": 8.98 }, "macos": { ... } } }, ...]
}
```

The headline tiles use `threads: "all"`, `set: "popular"`. Regenerate the sample from the
tables in `docs/benchmarks.md` with `node scripts/make-sample-chart-data.mjs`.

## Tests

```sh
npm run build && npm run serve &                 # :4173, production headers
CORPUS=/path/to/corpus npm test                  # Chromium: the sample, 11 files (CR3, NEF, ARW, RAF, DNG incl.
                                                 # JPEG XL and portrait, ORF, RW2, 100 MP RAF), sliders,
                                                 # zoom, preview, downloads, keyboard, mobile viewport
node tests/e2e.mjs --browsers firefox,webkit     # smaller set per engine
node scripts/serve.mjs --port 4174 --coep off &
node scripts/serve.mjs --port 4175 --coep credentialless &
node tests/modes.mjs --browsers chromium,firefox --widget   # headers, CPU renderer, widget
node tests/responsive.mjs --browsers chromium,firefox,webkit  # no horizontal overflow at 320-768 px
node tests/look.mjs                              # default look vs embedded previews (+ screenshots)
```

Screenshots go to `test-results/` (`OUT=<dir>` to change). `DEMO_WEBKIT=<path>` (or
`DEMO_FIREFOX`, `DEMO_CHROMIUM`) points Playwright at another browser executable.

## Peeking widget

Added before `</body>` as its guide (<https://peeking.ybouane.com/readme.md>) describes, with
`introDelayMs: 15000` (first peek after 15 s) and `crossorigin="anonymous"` on the script so it
loads under `require-corp`. It is a page-level singleton; nothing else to configure.
