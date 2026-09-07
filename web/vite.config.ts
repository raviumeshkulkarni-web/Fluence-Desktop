import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import path from 'node:path';

// Fluence Windows — React migration shell (main window + first-run wizard).
//
// - `base: './'` keeps asset URLs relative so each bundle works under
//   Tauri's asset protocol AND from the src/dist subpath in dev.
// - `outDir: ../src/dist` lets the React builds coexist with the vanilla
//   files in src/ (overlay.html, js/, css/) which keep working untouched.
// - Two HTML entries: index.html (main window) first, wizard.html
//   (first-run window, loaded via the src/wizard.html shim because the
//   frozen window URL resolves at the app root, not under dist/).
// - Tailwind is deliberately NOT adopted: the legacy stylesheet layer
//   (design tokens + global + per-window CSS) is reused verbatim via
//   app.css (main) and wizard/wizard.css (wizard), and Tailwind preflight
//   would fight it.
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
    rollupOptions: {
      input: {
        main: path.resolve(__dirname, 'index.html'),
        wizard: path.resolve(__dirname, 'wizard.html'),
      },
    },
  },
  server: {
    port: 1420,
    strictPort: true,
  },
});
