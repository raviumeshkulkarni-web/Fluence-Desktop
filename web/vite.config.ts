import { defineConfig, type Plugin } from 'vite';
import react from '@vitejs/plugin-react';
import path from 'node:path';
import fs from 'node:fs';

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

function fluenceDevPlugin(): Plugin {
  return {
    name: 'fluence-dev-server',
    configureServer(server) {
      // 1. Rewrite /dist/ entry paths to Vite root entry points
      server.middlewares.use((req, _res, next) => {
        if (req.url) {
          if (req.url === '/dist/index.html' || req.url.startsWith('/dist/index.html?')) {
            req.url = req.url.replace('/dist/index.html', '/index.html');
          } else if (req.url === '/dist/wizard.html' || req.url.startsWith('/dist/wizard.html?')) {
            req.url = req.url.replace('/dist/wizard.html', '/wizard.html');
          }
        }
        next();
      });

      // 2. Serve vanilla overlay and static assets from ../src
      server.middlewares.use((req, res, next) => {
        if (!req.url) return next();
        const rawUrl = req.url.split('?')[0];
        const staticPrefixes = ['/css/', '/js/', '/fonts/'];
        if (rawUrl === '/overlay.html' || staticPrefixes.some((p) => rawUrl.startsWith(p))) {
          const filePath = path.resolve(__dirname, '../src', '.' + rawUrl);
          if (fs.existsSync(filePath) && fs.statSync(filePath).isFile()) {
            const ext = path.extname(filePath).toLowerCase();
            const mimeTypes: Record<string, string> = {
              '.html': 'text/html; charset=utf-8',
              '.js': 'application/javascript; charset=utf-8',
              '.css': 'text/css; charset=utf-8',
              '.json': 'application/json; charset=utf-8',
              '.png': 'image/png',
              '.svg': 'image/svg+xml',
              '.woff2': 'font/woff2',
              '.woff': 'font/woff',
              '.ttf': 'font/ttf',
            };
            res.setHeader('Content-Type', mimeTypes[ext] || 'application/octet-stream');
            res.setHeader('Cache-Control', 'no-cache');
            return fs.createReadStream(filePath).pipe(res);
          }
        }
        next();
      });
    },
    transformIndexHtml(html, ctx) {
      // In dev mode (ctx.server is defined), allow Vite HMR WebSocket connection.
      // In prod build (ctx.server is undefined), production CSP is untouched.
      if (ctx.server) {
        return html.replace(
          /connect-src\s+([^;]+);/,
          'connect-src $1 ws://localhost:1420 http://localhost:1420;',
        );
      }
      return html;
    },
  };
}

export default defineConfig({
  base: './',
  plugins: [react(), fluenceDevPlugin()],
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
    fs: {
      allow: ['..'],
    },
  },
});
