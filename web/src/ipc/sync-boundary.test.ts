// Guards the Phase A remediation wiring end-to-end (audit findings C2/C3).
//
// The scheduler's fatal latch (`wait_for_command`) is only cleared by an
// explicit pass request (RunNow) or re-enabling sync. The production "Sync
// Now" control previously routed to `sync_toggle`, an enable/disable action
// that cannot re-arm the scheduler — so after a fatal error the button
// promised "Sync will run shortly" forever. These assertions fail if that
// wiring regresses.
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

test('ipc/sync.ts exposes runSyncNow bound to the sync_run_now command', () => {
  const src = readRepo('ipc/sync.ts');
  assert.match(
    src,
    /export const runSyncNow = \(\) =>\s*invokeCmd<SyncStatus>\('sync_run_now'\)/,
  );
});

test('SyncPage Sync Now invokes sync_run_now, not sync_toggle', () => {
  const page = readRepo('routes/SyncPage.tsx');
  assert.match(page, /await runSyncNow\(\)/);
  // The old pass-request-via-toggle wiring must not return: it cannot clear
  // the fatal latch and left the scheduler permanently paused.
  assert.doesNotMatch(page, /triggerSyncNow/);
});

test('legacy settings.js Sync Now invokes sync_run_now', () => {
  const js = readRepoRoot(join('src', 'js', 'settings.js'));
  assert.match(js, /invoke\('sync_run_now'\)/);
  assert.doesNotMatch(
    js,
    /invoke\('sync_toggle', \{ enabled: syncStatus/,
  );
});

test('SyncStatus models waiting_for_retry and both UIs render it honestly', () => {
  const ts = readRepo('ipc/sync.ts');
  assert.match(ts, /waiting_for_retry\?: boolean/);

  const page = readRepo('routes/SyncPage.tsx');
  assert.match(page, /s\.waiting_for_retry/);
  assert.match(page, /Automatic sync paused after an error/);

  const js = readRepoRoot(join('src', 'js', 'settings.js'));
  assert.match(js, /s\.waiting_for_retry/);
  assert.match(js, /Automatic sync paused after an error/);
});

test('SyncStatus models degraded and both UIs render it as a warning, not an error', () => {
  const ts = readRepo('ipc/sync.ts');
  assert.match(ts, /degraded\?: boolean/);
  assert.match(ts, /degraded_detail\?: string \| null/);

  const page = readRepo('routes/SyncPage.tsx');
  assert.match(page, /s\.degraded/);
  assert.match(page, /Partial sync/);

  const js = readRepoRoot(join('src', 'js', 'settings.js'));
  assert.match(js, /s\.degraded/);
  assert.match(js, /Partial sync/);
});
