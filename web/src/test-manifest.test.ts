// Guards against a SILENT GREEN test run.
//
// `node --test "src/**/*.test.ts"` exits 0 when the glob matches nothing, so a
// moved or renamed test file would look like a pass while running zero tests.
// This asserts the suite actually discovered tests, and fails loudly otherwise.

import test from 'node:test';
import assert from 'node:assert/strict';
import { readdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

// Scoped to `src/` — walking `web/` would descend into node_modules.
const SRC = dirname(fileURLToPath(import.meta.url));

function testFiles(dir: string): string[] {
  const out: string[] = [];
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = join(dir, entry.name);
    if (entry.isDirectory()) out.push(...testFiles(full));
    else if (entry.name.endsWith('.test.ts')) out.push(full);
  }
  return out;
}

test('the test glob matches the test files that exist', () => {
  const files = testFiles(SRC);
  assert.ok(
    files.length > 0,
    'no *.test.ts found under web/src — the npm test glob would silently match nothing',
  );
  // Names are asserted so a rename cannot quietly drop a suite.
  // Normalise separators: Windows yields backslashes, the assertions use `/`.
  const names = files.map((f) => f.slice(SRC.length).replace(/\\/g, '/')).sort();
  assert.ok(
    names.some((n) => n.endsWith('lib/claim.test.ts')),
    'claim messaging tests are missing from the glob',
  );
  assert.ok(
    names.some((n) => n.endsWith('ipc/claim-boundary.test.ts')),
    'claim IPC boundary tests are missing from the glob',
  );
});