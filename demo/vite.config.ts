import { defineConfig, type Plugin } from 'vite';
import { existsSync, readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { join } from 'node:path';

const here = fileURLToPath(new URL('.', import.meta.url));
const repo = join(here, '..');

/**
 * Cross-origin isolation (needed for the multithreaded WASM build). `require-corp`, as in
 * production: every cross-origin resource (only the peeking widget) sends CORP or CORS headers,
 * and Safari supports it (see README.md, "Hosting headers"). `DEMO_COEP=off` serves the page
 * without isolation (single-threaded path), `DEMO_COEP=credentialless` the lighter policy.
 */
const coep = process.env.DEMO_COEP ?? 'require-corp';
const isolationHeaders: Record<string, string> =
  coep === 'off'
    ? {}
    : { 'Cross-Origin-Opener-Policy': 'same-origin', 'Cross-Origin-Embedder-Policy': coep };

/** Brand assets, copied from `../branding/` to the site root. */
const BRAND_FILES = [
  'logo.svg',
  'logo-dark.svg',
  'logo-mark.svg',
  'favicon.svg',
  'favicon.ico',
  'favicon-32.png',
  'apple-touch-icon.png',
  'icon-192.png',
  'icon-512.png',
  'icon-512-maskable.png',
  'og-banner.png',
];
const MIME: Record<string, string> = { svg: 'image/svg+xml', png: 'image/png', ico: 'image/x-icon' };

/**
 * Leaves the package's WebGPU WASM build out of the site: the demo never selects it (see
 * src/app.ts, "engine"), but the package references every build with literal URLs, so bundlers
 * emit them all. Its glue module becomes a stub that throws, and its .wasm is not emitted.
 */
function dropWebGpuBuild(): Plugin {
  const stub = '\0flashraw-no-webgpu-build';
  return {
    name: 'flashraw-drop-webgpu-build',
    apply: 'build',
    enforce: 'pre',
    resolveId: (id) => (/(^|\/)flashraw_bindgen_gpu\.js$/.test(id) ? stub : null),
    load: (id) => (id === stub ? "throw new Error('the WebGPU build is not part of this site');" : null),
    generateBundle(_, bundle) {
      for (const name of Object.keys(bundle)) if (/(^|\/)flashraw_simd_gpu-[^/]*\.wasm$/.test(name)) delete bundle[name];
    },
  };
}

const brandPath = (name: string) => join(repo, 'branding', name);

function brandAssets(): Plugin {
  return {
    name: 'flashraw-brand-assets',
    configureServer(server) {
      server.middlewares.use((req, res, next) => {
        const name = (req.url ?? '').split('?')[0].replace(/^\//, '');
        if (!BRAND_FILES.includes(name)) return next();
        res.setHeader('Content-Type', MIME[name.split('.').pop()!]);
        res.end(readFileSync(brandPath(name)));
      });
    },
    generateBundle() {
      const missing = BRAND_FILES.filter((n) => !existsSync(brandPath(n)));
      if (missing.length) this.error(`missing brand assets in branding/: ${missing.join(', ')}`);
      for (const name of BRAND_FILES) {
        this.emitFile({ type: 'asset', fileName: name, source: readFileSync(brandPath(name)) });
      }
    },
  };
}

/** `virtual:charts-data`: docs/charts/data.json (from tools/bench/charts.py) in the ChartsData schema. */
function chartsData(): Plugin {
  const file = join(repo, 'docs', 'charts', 'data.json');
  const id = 'virtual:charts-data';
  return {
    name: 'flashraw-charts-data',
    resolveId: (s) => (s === id ? '\0' + id : null),
    load(s) {
      if (s !== '\0' + id) return null;
      this.addWatchFile(file);
      const raw = JSON.parse(readFileSync(file, 'utf8'));
      return `export default ${JSON.stringify(fromBenchData(raw))};`;
    },
  };
}

const MACHINES: Record<string, { name: string; icon: string }> = {
  linux: { name: 'Linux x86-64', icon: '🐧' },
  macos: { name: 'macOS Apple Silicon', icon: '🍎' },
};

/** Converts docs/charts/data.json (written by tools/bench/charts.py) to the ChartsData schema. */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
function fromBenchData(d: any) {
  const machines = Object.keys(d.machines);
  const ms = (r: any, lib: string): number | null => r?.[lib]?.ms ?? null;
  const headline = [];
  for (const m of machines)
    for (const [threads, key] of [['all', 'all'], ['1', '1']] as const)
      for (const metric of ['decode', 'rgb'] as const)
        for (const [vs, v] of Object.entries<any>(d.summary[m]?.[key]?.[metric] ?? {}))
          headline.push({ metric, vs, threads, set: 'popular', machine: m, geomean: v.geomean_speedup, median: v.median_speedup, wins: v.flashraw_faster, files: v.files });
  const libs = ['flashraw', 'libraw', 'rawspeed'];
  const cameras = d.per_camera.flatMap((c: any) => machines.filter((m) => c[m]).map((m) => ({
    machine: m,
    camera: c.note ? `${c.camera} (${c.note})` : c.camera,
    format: c.format,
    codec: c.compression,
    mp: c.megapixels == null ? null : Math.round(c.megapixels * 10) / 10,
    decode: Object.fromEntries(libs.map((l) => [l, ms(c[m].decode, l)])),
    rgb: Object.fromEntries(libs.filter((l) => l !== 'rawspeed').map((l) => [l, ms(c[m].rgb, l)])),
    gpu: ms(c[m].rgb_gpu, 'flashraw'),
  })));
  // Per format and codec: geometric mean of the files' wall times.
  const geo = (xs: number[]) => (xs.length ? Math.round(Math.exp(xs.reduce((a, x) => a + Math.log(x), 0) / xs.length) * 100) / 100 : null);
  const groups = new Map<string, any[]>();
  for (const c of d.per_camera) {
    const k = `${c.format}\t${c.compression}`;
    groups.set(k, [...(groups.get(k) ?? []), c]);
  }
  const formats = [...groups].map(([k, cs]) => {
    const [format, codec] = k.split('\t');
    return {
      format, codec, files: cs.length,
      ms: Object.fromEntries(machines.map((m) => [m, Object.fromEntries((['decode', 'rgb'] as const).map((metric) => [metric,
        Object.fromEntries(libs.map((l) => [l, geo(cs.map((c) => ms(c[m]?.[metric], l)).filter((v): v is number => v != null))]))]))])),
    };
  }).sort((a, b) => a.format.localeCompare(b.format) || a.codec.localeCompare(b.codec));
  return {
    version: 1,
    source: d.method,
    description: `Times in ms (lower is better), all cores, median of runs over ${d.popular_cameras} popular cameras; speedups = other library time / FlashRaw time.`,
    libraries: Object.entries(d.libraries).map(([id, name]) => ({ id, name })),
    machines: machines.map((id) => ({ id, name: MACHINES[id]?.name ?? id, cpu: d.machines[id], icon: MACHINES[id]?.icon })),
    headline, cameras, formats,
  };
}

/**
 * Preload the roman text face (headline and body) so it is usually in place before the first
 * paint: the swap from the fallback would otherwise shift the hero.
 */
function preloadFont(): Plugin {
  return {
    name: 'flashraw-preload-font',
    transformIndexHtml(html, ctx) {
      // roman face only: preloading the italic one (the "RAW" in the headline) measured ~90 ms later first paint on a 3 Mbit link
      const files = [/newsreader-core.*\.woff2$/].map((re) => Object.keys(ctx.bundle ?? {}).find((f) => re.test(f)));
      return files.flatMap((file) => (file ? [{ tag: 'link', attrs: { rel: 'preload', href: `/${file}`, as: 'font', type: 'font/woff2', crossorigin: '' }, injectTo: 'head-prepend' as const }] : []));
    },
  };
}

export default defineConfig({
  plugins: [brandAssets(), dropWebGpuBuild(), chartsData(), preloadFont()],
  optimizeDeps: { exclude: ['flashraw'] },
  worker: { format: 'es' },
  server: { headers: isolationHeaders, fs: { allow: [repo] } },
  preview: { headers: isolationHeaders },
  build: {
    target: 'es2022',
    assetsInlineLimit: 0,
    modulePreload: { polyfill: false },
  },
});
