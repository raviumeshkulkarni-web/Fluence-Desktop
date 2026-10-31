// Audit #27: the main-window capability carried an UNUSED, over-broad `fs`
// grant.
//
// `src-tauri/capabilities/main.json` granted `fs:allow-read`, `fs:allow-exists`
// and an `fs:scope` allowlist naming ten paths under `$APPLOCALDATA/Fluence/`
// — including `settings.json` (plaintext dictionary + snippets) and
// `sync-oauth.json` (the OAuth client secret). Nothing in the renderer used
// it: no `plugin-fs` import, no `readTextFile`, no `plugin:fs|*` invoke, in
// web/src, in the legacy src/js, or in the shipped `src/dist/assets/main-*.js`.
// `sync-oauth.json` is read server-side by Rust (`oauth_config_path()` in
// src-tauri/src/sync/scheduler.rs).
//
// The grant was therefore a standing capability: any future renderer-level
// injection, or any dependency added to the main bundle, would turn into direct
// exfiltration of those files with no further permission change.
//
// These assertions fail loudly if it comes back — either as a grant, or as
// frontend code that would make the grant necessary again.
// Run: node --test tests/capability-fs-grants.test.mjs

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync, readdirSync, statSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

const ROOT = path.dirname(path.dirname(fileURLToPath(import.meta.url)));

function read(rel) {
  return readFileSync(path.join(ROOT, rel), 'utf8');
}

const mainCap = read('src-tauri/capabilities/main.json');
const syncPage = read('web/src/routes/SyncPage.tsx');

test('the main-window capability parses as JSON (a malformed edit fails here, not at runtime)', () => {
  const parsed = JSON.parse(mainCap);
  assert.equal(parsed.identifier, 'main-capability');
  assert.deepEqual(parsed.windows, ['main']);
  assert.ok(Array.isArray(parsed.permissions));
});

test('the main-window capability grants no fs permission', () => {
  const permissions = JSON.parse(mainCap).permissions;
  for (const entry of permissions) {
    const id = typeof entry === 'string' ? entry : entry.identifier;
    assert.doesNotMatch(
      id,
      /^fs:/,
      `fs capability "${id}" is granted to the main window but no frontend code uses it`,
    );
  }
  // Belt-and-braces on the raw text, so a commented-out or half-deleted block
  // is still caught even if it somehow stops parsing as an entry.
  assert.doesNotMatch(mainCap, /fs:allow-read/);
  assert.doesNotMatch(mainCap, /fs:allow-exists/);
  assert.doesNotMatch(mainCap, /fs:scope/);
});

test('the removed scope named no secret material', () => {
  // Documents exactly what was withdrawn, so the blast radius of a future
  // re-grant is readable at the point of the re-grant.
  assert.doesNotMatch(mainCap, /sync-oauth\.json/);
  assert.doesNotMatch(mainCap, /settings\.json/);
});

test('the other capabilities never granted fs either', () => {
  for (const rel of [
    'src-tauri/capabilities/overlay.json',
    'src-tauri/capabilities/wizard.json',
  ]) {
    assert.doesNotMatch(read(rel), /"fs:/, `${rel} grants fs access`);
  }
});

test('the surviving main-window permissions are unchanged', () => {
  // Removing fs must not have taken anything else with it.
  const permissions = JSON.parse(mainCap).permissions;
  for (const expected of [
    'core:default',
    'updater:default',
    'process:default',
    'process:allow-restart',
    'dialog:allow-open',
  ]) {
    assert.ok(
      permissions.includes(expected),
      `expected "${expected}" to still be granted to the main window`,
    );
  }
  // No capability may be added as a side effect of this cleanup.
  assert.equal(permissions.length, 5, 'the main capability gained a permission');
});

// --- the half of the rule that keeps the removal honest ---------------------

function filesUnder(relDir) {
  const out = [];
  const walk = (dir) => {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      if (entry.name === 'node_modules') continue;
      const full = path.join(dir, entry.name);
      if (entry.isDirectory()) walk(full);
      else out.push(full);
    }
  };
  walk(path.join(ROOT, relDir));
  return out;
}

const FS_USAGE = [
  /@tauri-apps\/plugin-fs/,
  /plugin:\|?read_text_file|plugin:\|?exists|plugin:\|?read_dir/,
  /plugin:fs/,
  /\breadTextFile\b/,
  /\bwriteTextFile\b/,
  /\breadBinaryFile\b/,
];

test('no frontend source uses the fs plugin (which is why the grant was removable)', () => {
  const sources = [
    ...filesUnder('web/src'),
    ...filesUnder('src/js'),
  ];
  assert.ok(sources.length > 20, 'source walk found suspiciously few files');
  for (const file of sources) {
    if (!/\.(ts|tsx|js|mjs|jsx)$/.test(file)) continue;
    const text = readFileSync(file, 'utf8');
    for (const pattern of FS_USAGE) {
      assert.doesNotMatch(
        text,
        pattern,
        `${path.relative(ROOT, file)} uses the fs plugin — the main-window fs grant would then be required and must be re-added deliberately with a scope`,
      );
    }
  }
});

test('the shipped bundle carries no fs plugin call', () => {
  // The bundle is a build artifact; skip when it has not been built.
  const assets = path.join(ROOT, 'src/dist/assets');
  let entries;
  try {
    entries = readdirSync(assets).filter((f) => f.endsWith('.js'));
  } catch {
    return; // not built in this checkout
  }
  if (entries.length === 0) return;
  for (const entry of entries) {
    const full = path.join(assets, entry);
    if (statSync(full).size > 8 * 1024 * 1024) continue;
    const text = readFileSync(full, 'utf8');
    for (const pattern of FS_USAGE) {
      assert.doesNotMatch(
        text,
        pattern,
        `src/dist/assets/${entry} calls the fs plugin — the capability removal would break it`,
      );
    }
  }
});

test('SyncPage reads sync state over IPC, not the filesystem', () => {
  // The concrete surface the withdrawn scope protected: the OAuth client
  // secret file. It must be reachable only through a Tauri command.
  assert.match(syncPage, /from '@\/ipc\/sync'/);
  assert.doesNotMatch(syncPage, /sync-oauth/);
});
