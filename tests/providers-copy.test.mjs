import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

const ROOT = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const providers = readFileSync(path.join(ROOT, 'web/src/routes/ProvidersPage.tsx'), 'utf8');
const sidebar = readFileSync(path.join(ROOT, 'web/src/components/fluence/Sidebar.tsx'), 'utf8');

test('provider tabs explain what each model does in simple language', () => {
  assert.match(providers, /description="Choose the model that handles Agent Mode commands and text actions\."/);
  assert.match(providers, /description="Choose the model that cleans up your transcriptions after dictation\."/);
});

test('sidebar uses the simpler AI Cleanup label', () => {
  assert.match(sidebar, /page: 'formatting', label: 'AI Cleanup'/);
  assert.doesNotMatch(sidebar, /AI Post Processing/);
});
