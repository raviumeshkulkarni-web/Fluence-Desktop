// Node test resolver: maps the Vite `@/` alias onto `src/` and lets Node's native
// type-stripping resolve the `.ts` modules the app is written in.
//
// Needed because the app's imports are alias-based (`@/ipc/tauri`) for the bundler,
// which Node does not know about. Keeps `npm test` dependency-free: no vitest,
// no jsdom, no transform pipeline.

import { existsSync } from 'node:fs';
import { fileURLToPath, pathToFileURL } from 'node:url';

const SRC = new URL('./src/', import.meta.url);

export async function resolve(specifier, context, next) {
  if (specifier.startsWith('@/')) {
    const rel = specifier.slice(2);
    const candidates = [rel, `${rel}.ts`, `${rel}.tsx`, `${rel}/index.ts`];
    for (const candidate of candidates) {
      const url = new URL(candidate, SRC);
      if (existsSync(fileURLToPath(url))) {
        return next(url.href, context);
      }
    }
    return next(pathToFileURL(new URL(rel, SRC).pathname).href, context);
  }
  // Test files import each other with an explicit `.ts` extension (required by
  // Node's type-stripping); leave everything else to the default resolver.
  return next(specifier, context);
}