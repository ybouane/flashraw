import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

// Cross-origin isolation lets flashraw use its multithreaded build (SharedArrayBuffer). Without
// these headers everything still works, single-threaded. Production servers must send the same
// two headers (see the flashraw README, "Multithreading").
const isolation = {
  'Cross-Origin-Opener-Policy': 'same-origin',
  'Cross-Origin-Embedder-Policy': 'require-corp',
};

export default defineConfig({
  plugins: [react()],
  // Keep flashraw out of dependency pre-bundling so its `new URL('./*.wasm', import.meta.url)`
  // references resolve to the package files in dev mode (production builds handle it anyway).
  optimizeDeps: { exclude: ['flashraw'] },
  worker: { format: 'es' },
  server: { headers: isolation },
  preview: { headers: isolation },
});
