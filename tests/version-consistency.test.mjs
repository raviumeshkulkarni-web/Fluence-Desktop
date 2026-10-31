// Release integrity: the four version declarations that ship together must
// always agree. They drifted once before (web/package.json / Cargo.toml at
// 1.25.1 while tauri.conf.json said 1.27.1), which is exactly the class of
// silent inconsistency this test exists to fail loudly on.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');

function parseJson(rel) {
  return JSON.parse(readFileSync(join(root, rel), 'utf8'));
}

function cargoVersion() {
  const toml = readFileSync(join(root, 'src-tauri', 'Cargo.toml'), 'utf8');
  const match = toml.match(/(^|\n)\s*version\s*=\s*"([^"]+)"/);
  assert.ok(match, 'Cargo.toml must declare a package version');
  return match[2];
}

function lockVersion() {
  const lock = readFileSync(join(root, 'src-tauri', 'Cargo.lock'), 'utf8');
  const match = lock.match(/name = "fluence-windows"\r?\nversion = "([^"]+)"/);
  assert.ok(match, 'Cargo.lock must contain the fluence-windows package');
  return match[1];
}

test('all four version declarations agree', () => {
  const versions = {
    'src-tauri/Cargo.toml': cargoVersion(),
    'src-tauri/Cargo.lock': lockVersion(),
    'root package.json': parseJson('package.json').version,
    'web/package.json': parseJson('web/package.json').version,
    'src-tauri/tauri.conf.json': parseJson('src-tauri/tauri.conf.json').version,
  };
  const unique = new Set(Object.values(versions));
  assert.ok(
    unique.size === 1,
    `all release surfaces must report the same version, got: ${JSON.stringify(versions)}`,
  );
  const v = Object.values(versions)[0];
  assert.match(v, /^\d+\.\d+\.\d+$/, `version must be semver-ish, got ${v}`);
});

// Release notes exist in exactly ONE place (.github/RELEASE_BODY.md). An
// earlier state had the same narrative duplicated inline in release.yml,
// so editing one left the other publishing stale prose — and a HARD
// coded future tag would re-publish this release's story by accident.
// There must be no inline narrative block and no "keep in sync" contract.
test('release notes have a single source of truth', () => {
  const workflow = readFileSync(
    join(root, '.github', 'workflows', 'release.yml'),
    'utf8',
  );
  const bodyFile = readFileSync(join(root, '.github', 'RELEASE_BODY.md'), 'utf8');

  assert.match(
    workflow,
    /releaseBody:\s*\$\{\{\s*steps\.notes\.outputs\.body\s*\}\}/,
    'releaseBody must come from the notes-extraction step',
  );
  assert.ok(
    workflow.includes('.github/RELEASE_BODY.md'),
    'workflow must name the single source file',
  );
  assert.ok(
    !workflow.includes("releaseBody: |"),
    'releaseBody must not carry an inline narrative block',
  );
  // Release-agnostic: the workflow must never carry the narrative itself.
  // Previously this pinned one sentence from a specific release, so it broke
  // on every legitimate notes rewrite - which is exactly when you least want
  // a red test. Compare against the CURRENT file's own headings instead.
  const headings = bodyFile
    .split('\n')
    .map((l) => l.trim())
    .filter((l) => l.startsWith('#'));
  assert.ok(headings.length > 0, 'release notes must have at least one heading');
  for (const h of headings) {
    assert.ok(
      !workflow.includes(h),
      `workflow must not duplicate release-note heading: ${h}`,
    );
  }
  assert.ok(
    !workflow.includes('Keep in sync'),
    'no dual-copy contract: editors must only touch RELEASE_BODY.md',
  );
  assert.ok(
    bodyFile.includes('steps:notes') || workflow.includes('id: notes'),
    'notes file references or workflow declares the extraction step id',
  );
});
