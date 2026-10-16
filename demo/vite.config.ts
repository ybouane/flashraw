import { defineConfig, type Plugin } from 'vite';
import { existsSync, readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { join } from 'node:path';

const here = fileURLToPath(new URL('.', import.meta.url));
const repo = join(here, '..');

/**
 * Cross-origin isolation (needed for the multithreaded WASM build). `credentialless`, as in
 * production: the peeking widget's analytics beacon has no CORP header, which `require-corp`
 * blocks (see README.md, "Hosting headers"). `DEMO_COEP=off` serves the page without isolation
 * (single-threaded path), `DEMO_COEP=require-corp` the stricter policy.
 */
const coep = process.env.DEMO_COEP ?? 'credentialless';
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

/**
 * `virtual:charts-data`: docs/charts/data.json when present (drop-in), else the documented
 * sample in src/charts-data.sample.json.
 */
function chartsData(): Plugin {
  const real = join(repo, 'docs', 'charts', 'data.json');
  const sample = join(here, 'src', 'charts-data.sample.json');
  const id = 'virtual:charts-data';
  return {
    name: 'flashraw-charts-data',
    resolveId: (s) => (s === id ? '\0' + id : null),
    load(s) {
      if (s !== '\0' + id) return null;
      const file = existsSync(real) ? real : sample;
      this.addWatchFile(file);
      if (file === sample) this.warn('docs/charts/data.json not found: charts use src/charts-data.sample.json');
      return `export default ${readFileSync(file, 'utf8')};`;
    },
  };
}

export default defineConfig({
  plugins: [brandAssets(), chartsData()],
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
