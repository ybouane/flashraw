// Browser / Web Worker / Deno entry point.
//
// The .wasm binaries are referenced with literal `new URL('./x.wasm', import.meta.url)`
// expressions so Vite, webpack 5, Rollup and Parcel emit them as assets automatically. With
// bundlers that do not (esbuild), copy `dist/*.wasm` next to your bundle or pass
// `init({ baseUrl })` / `init({ wasm })`.

import { _setPlatform } from './core.js';

function wasmUrl(variant, options) {
  const file = variant === 'simd' ? 'flashraw_simd.wasm' : 'flashraw.wasm';
  if (options.baseUrl) return new URL(file, options.baseUrl);
  return variant === 'simd'
    ? new URL('./flashraw_simd.wasm', import.meta.url)
    : new URL('./flashraw.wasm', import.meta.url);
}

_setPlatform({
  name: typeof window === 'undefined' && typeof self !== 'undefined' ? 'worker' : 'browser',
  // fetch() + instantiateStreaming compiles while downloading (served as application/wasm).
  load: (variant, options) => fetch(wasmUrl(variant, options)),
});

export * from './core.js';
