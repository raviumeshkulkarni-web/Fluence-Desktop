// Shared helper: extract a top-level `function NAME(...) {...}` span from a
// shipped source file so node:test asserts the REAL function text (no copies).
// Both overlay.js and settings.js run in a browser context (top-level
// `window`/`document`), so they cannot be required in Node — but the tested
// functions are pure and dependency-free, making source extraction exact.
import { readFileSync } from 'node:fs';

export function extractFunction(sourcePath, name) {
  const src = readFileSync(sourcePath, 'utf8');
  const marker = `function ${name}(`;
  const start = src.indexOf(marker);
  if (start === -1) throw new Error(`${name} not found in ${sourcePath}`);
  const bodyStart = src.indexOf('{', start);
  if (bodyStart === -1) throw new Error(`${name} has no body in ${sourcePath}`);
  let depth = 0;
  for (let i = bodyStart; i < src.length; i++) {
    const ch = src[i];
    if (ch === '{') depth++;
    else if (ch === '}') {
      depth--;
      if (depth === 0) return src.slice(start, i + 1);
    }
  }
  throw new Error(`unbalanced braces in ${name} (${sourcePath})`);
}

// Evaluate one or more extracted plain-JS function sources together and
// return the requested binding (later functions may call earlier ones).
export function evalFunctions(sources, exportName) {
  const factory = new Function(`${sources.join('\n')}; return ${exportName};`);
  return factory();
}
