// Guards the dictionary import IPC contract (review follow-up to Phase B).
//
// `import_dictionary` keeps its legacy contract: it returns the added count as
// a bare number, so any out-of-tree caller keeps working. The reporting
// variant `import_dictionary_report` returns `{ added, coerced_kind }` and is
// the only path both UIs may use. These assertions fail if the wiring drifts
// back to the shape-changing call or if the legacy contract is altered.
import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const SRC = dirname(fileURLToPath(import.meta.url));

function readRepo(relFromWebSrc: string): string {
  return readFileSync(join(SRC, '..', relFromWebSrc), 'utf8');
}

function readRepoRoot(rel: string): string {
  // web/src/ipc -> repo root (web/../.. == Fluence-Windows)
  return readFileSync(join(SRC, '..', '..', '..', rel), 'utf8');
}

test('ipc/dictionary.ts imports via import_dictionary_report with the result shape', () => {
  const src = readRepo('ipc/dictionary.ts');
  assert.match(
    src,
    /invokeCmd<DictionaryImportResult>\('import_dictionary_report'/,
  );
  assert.match(src, /added: number;/);
  assert.match(src, /coerced_kind: number;/);
});

test('ipc/dictionary.ts never calls the legacy import_dictionary command', () => {
  const src = readRepo('ipc/dictionary.ts');
  assert.doesNotMatch(src, /'import_dictionary'/);
});

test('legacy settings.js imports via import_dictionary_report and reads the shape', () => {
  const js = readRepoRoot(join('src', 'js', 'settings.js'));
  assert.match(js, /invoke\('import_dictionary_report'/);
  assert.match(js, /coerced_kind/);
  assert.doesNotMatch(js, /invoke\('import_dictionary', \{ jsonData/);
});

test('Rust keeps the legacy import_dictionary usize contract', () => {
  const rs = readRepoRoot(join('src-tauri', 'src', 'dictionary.rs'));
  assert.match(
    rs,
    /pub fn import_dictionary\(\s*window: tauri::Window,\s*json_data: String,\s*scheduler:[^)]+\) -> Result<usize, String>/,
  );
  assert.match(rs, /pub fn import_dictionary_report\(/);
});
