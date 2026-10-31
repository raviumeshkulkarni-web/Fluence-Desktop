// Installs the `@/` → `src/` resolver for `node --test`. See test-resolver.mjs.
import { register } from 'node:module';

register('./test-resolver.mjs', import.meta.url);