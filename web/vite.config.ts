import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import path from 'node:path';

// Fluence Windows — React migration shell (main window only).
//
// - `base: './'` keeps asset URLs relative so the bundle works under
//   Tauri's asset protocol AND from the src/dist subpath in dev.
// - `outDir: ../src/dist` lets the React build coexist with the vanilla
//   files in src/ (overlay.html, wizard.html, js/, css/) which keep
//   working untouched until their windows migrate.
// - Tailwind is deliberately NOT adopted in this slice: the 100KB legacy
//   stylesheet layer (design tokens + global + per-window CSS) is reused
//   verbatim via app.css, and Tailwind preflight would fight it.
//   Revisit when more shadcn primitives land.
export default defineConfig({
  base: './',
  plugins: [react()],
  resolve: {
    alias: {
      '@': path.resolve(__dirname, 'src'),
    },
  },
  build: {
    outDir: '../src/dist',
    emptyOutDir: true,
    target: 'es2020',
    sourcemap: false,
  },
  server: {
    port: 1420,
    strictPort: true,
  },
});
