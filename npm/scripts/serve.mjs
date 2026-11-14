#!/usr/bin/env node
// Minimal static file server for the browser demos and the Playwright test.
//
//   node scripts/serve.mjs [--port 8080] [--root <dir>] [--isolated]
//
// Serves `npm/` by default (so /dist, /examples and /tests/fixtures are reachable). With
// --isolated it sends COOP/COEP headers (cross-origin isolation, needed for SharedArrayBuffer
// and any future wasm-threads build).

import { createServer } from 'node:http';
import { createReadStream, statSync } from 'node:fs';
import { extname, join, normalize, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';

const TYPES = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.mjs': 'text/javascript; charset=utf-8',
  '.css': 'text/css; charset=utf-8',
  '.json': 'application/json',
  '.wasm': 'application/wasm',
  '.jpg': 'image/jpeg',
  '.svg': 'image/svg+xml',
  '.map': 'application/json',
};

export function serve({ root, port = 0, isolated = false, extra = {} } = {}) {
  const defaultBase = resolve(root ?? fileURLToPath(new URL('..', import.meta.url)));
  const server = createServer((req, res) => {
    const url = new URL(req.url, 'http://x');
    // `extra` maps URL prefixes to other directories (e.g. { '/corpus/': '/data/corpus' }).
    let base = defaultBase;
    let rel = url.pathname;
    for (const [prefix, dir] of Object.entries(extra)) {
      if (rel.startsWith(prefix)) {
        base = resolve(dir);
        rel = rel.slice(prefix.length - 1);
        break;
      }
    }
    let path = normalize(join(base, decodeURIComponent(rel)));
    if (path !== base && !path.startsWith(base + sep)) {
      res.writeHead(403).end();
      return;
    }
    try {
      if (statSync(path).isDirectory()) path = join(path, 'index.html');
      const st = statSync(path);
      const headers = {
        'content-type': TYPES[extname(path).toLowerCase()] ?? 'application/octet-stream',
        'content-length': st.size,
        'cache-control': 'no-store',
      };
      if (isolated) {
        headers['cross-origin-opener-policy'] = 'same-origin';
        headers['cross-origin-embedder-policy'] = 'require-corp';
      }
      res.writeHead(200, headers);
      createReadStream(path).pipe(res);
    } catch {
      res.writeHead(404, { 'content-type': 'text/plain' }).end('not found');
    }
  });
  return new Promise((ok) => server.listen(port, '127.0.0.1', () => ok(server)));
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const arg = (k, d) => {
    const i = process.argv.indexOf(k);
    return i >= 0 ? process.argv[i + 1] : d;
  };
  const server = await serve({
    root: arg('--root'),
    port: Number(arg('--port', 8080)),
    isolated: process.argv.includes('--isolated'),
  });
  const { port } = server.address();
  console.log(`serving on http://127.0.0.1:${port}/  (demo: http://127.0.0.1:${port}/examples/browser/)`);
}
