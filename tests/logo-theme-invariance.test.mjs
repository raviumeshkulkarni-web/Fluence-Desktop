import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';

const ROOT = resolve(import.meta.dirname, '..');

test('Fluence wordmark preserves invariant cyan for "ence" across themes', () => {
  const tsx = readFileSync(
    resolve(ROOT, 'web/src/components/fluence/FluenceLogo.tsx'),
    'utf8'
  );

  // "ence" span must never bind to the mutable functional accent --color-brand-cyan
  assert.doesNotMatch(
    tsx,
    /flu<span[^>]*style=\{\{[^}]*color-brand-cyan[^}]*\}\}>ence<\/span>/,
    'FluenceWordmark must not use mutable --color-brand-cyan for "ence"'
  );

  // "ence" span must use --color-logo-cyan or fallback #0BD6E3
  assert.match(
    tsx,
    /flu<span[^>]*style=\{\{[^}]*color-logo-cyan[^}]*#0BD6E3[^}]*\}\}>ence<\/span>/,
    'FluenceWordmark must style "ence" with invariant --color-logo-cyan (#0BD6E3)'
  );
});

test('accent-themes.css keeps --color-logo-cyan constant across light mode and accent themes', () => {
  const css = readFileSync(
    resolve(ROOT, 'web/src/accent-themes.css'),
    'utf8'
  );

  // :root must define --color-logo-cyan as canonical brand cyan #0BD6E3
  assert.match(
    css,
    /--color-logo-cyan:\s*#0BD6E3;/,
    ':root must define --color-logo-cyan as #0BD6E3'
  );

  // [data-theme="light"] must not mutate --color-logo-cyan
  assert.doesNotMatch(
    css,
    /\[data-theme=['"]light['"]\][\s\S]*?--color-logo-cyan:\s*#0E7490;/,
    'Light theme must not mutate --color-logo-cyan'
  );
});

test('src/index.html wordmark preserves invariant brand cyan for "ence"', () => {
  const html = readFileSync(
    resolve(ROOT, 'src/index.html'),
    'utf8'
  );

  // No occurrence in index.html should use var(--color-brand-cyan) for ence
  assert.doesNotMatch(
    html,
    /flu<span style="[^"]*--color-brand-cyan[^"]*">ence<\/span>/,
    'src/index.html must not use mutable --color-brand-cyan for "ence"'
  );

  // All occurrences must use var(--color-logo-cyan, #0BD6E3)
  const matches = [...html.matchAll(/flu<span style="([^"]*)">ence<\/span>/g)];
  assert.ok(matches.length >= 2, 'Must find at least 2 occurrences of the wordmark');
  for (const match of matches) {
    assert.match(
      match[1],
      /--color-logo-cyan,\s*#0BD6E3/,
      'Wordmark in src/index.html must use --color-logo-cyan with #0BD6E3 fallback'
    );
  }
});
