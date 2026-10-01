import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

const ROOT = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const agents = readFileSync(path.join(ROOT, 'web/src/routes/AgentsPage.tsx'), 'utf8');
const css = readFileSync(path.join(ROOT, 'web/src/app.css'), 'utf8');

test('custom agent instructions use a two-line preview instead of expanding the card', () => {
  assert.match(agents, /className="selection-row-description agent-row-desc"/);
  assert.match(css, /\.agent-row-desc\s*\{[^}]*-webkit-line-clamp:\s*2;[^}]*overflow:\s*hidden;/s);
});
