import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

export default defineConfig({
  plugins: [react()],
  // Keep flashraw out of dependency pre-bundling so its `new URL('./*.wasm', import.meta.url)`
  // references resolve to the package files in dev mode (production builds handle it anyway).
  optimizeDeps: { exclude: ['flashraw'] },
  worker: { format: 'es' },
});
