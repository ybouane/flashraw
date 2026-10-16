// Minimal static server for dist/ with the production headers (no dependencies).
//   node scripts/serve.mjs [--port 4173] [--coep credentialless|require-corp|off] [--gzip]
// `--coep off` serves the page without cross-origin isolation (single-threaded WASM).

import { createServer } from 'node:http';
import { readFile, stat } from 'node:fs/promises';
import { extname, join, normalize } from 'node:path';
import { fileURLToPath } from 'node:url';
import { gzipSync } from 'node:zlib';

const args = process.argv.slice(2);
const opt = (name, def) => {
  const i = args.indexOf(`--${name}`);
  return i >= 0 ? args[i + 1] : def;
};
const port = Number(opt('port', 4173));
const coep = opt('coep', 'credentialless');
const gzip = args.includes('--gzip');
const root = fileURLToPath(new URL('../dist/', import.meta.url));

const TYPES = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.css': 'text/css; charset=utf-8',
  '.json': 'application/json',
  '.webmanifest': 'application/manifest+json',
  '.wasm': 'application/wasm',
  '.svg': 'image/svg+xml',
  '.png': 'image/png',
  '.ico': 'image/x-icon',
  '.woff2': 'font/woff2',
  '.xml': 'application/xml',
  '.txt': 'text/plain; charset=utf-8',
};

createServer(async (req, res) => {
  const url = new URL(req.url, 'http://x');
  let path = normalize(decodeURIComponent(url.pathname)).replace(/^(\.\.[/\\])+/, '');
  if (path.endsWith('/')) path += 'index.html';
  const file = join(root, path);
  try {
    if (!(await stat(file)).isFile()) throw new Error();
    let body = await readFile(file);
    const type = TYPES[extname(file)] ?? 'application/octet-stream';
    const headers = { 'Content-Type': type, 'X-Content-Type-Options': 'nosniff' };
    if (coep !== 'off') {
      headers['Cross-Origin-Opener-Policy'] = 'same-origin';
      headers['Cross-Origin-Embedder-Policy'] = coep;
    }
    if (path.startsWith('/assets/')) headers['Cache-Control'] = 'public, max-age=31536000, immutable';
    if (gzip && /text|javascript|json|wasm|svg|xml/.test(type) && /gzip/.test(req.headers['accept-encoding'] ?? '')) {
      body = gzipSync(body, { level: 6 });
      headers['Content-Encoding'] = 'gzip';
    }
    res.writeHead(200, headers);
    res.end(body);
  } catch {
    res.writeHead(404, { 'Content-Type': 'text/plain' });
    res.end('not found');
  }
}).listen(port, () => console.log(`serving ${root} on http://localhost:${port} (COEP: ${coep}${gzip ? ', gzip' : ''})`));
