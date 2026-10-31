// S7-rem tests: sign-in/out failure toasts must never render raw provider
// strings. Extracts the REAL `describeSignInError` from src/js/settings.js and
// asserts the mapping — same harness as credential-target-parity.test.mjs.
//
// AUDIT S7 (live-surface gap): the block above guards the LEGACY vanilla copy,
// which is NOT what ships. `tauri.conf.json` sets `frontendDist: "../src"` and
// serves `src/dist/index.html` (the Vite bundle), so web/src is the live tree —
// and the React page rendered the raw backend error while this suite stayed
// green. The second half of this file repeats the same assertions against the
// LIVE TS module and pins the call sites. The legacy assertions above are kept
// intact on purpose: the vanilla file is still in the tree.
// Run: node --test tests/signin-toast-sanitization.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { readFileSync } from 'node:fs';
import path from 'node:path';
import { extractFunction, evalFunctions } from './extract-js-function.mjs';

const ROOT = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const settingsPath = path.join(ROOT, 'src/js/settings.js');
const { describeSignInError } = evalFunctions(
  [extractFunction(settingsPath, 'describeSignInError')],
  '{ describeSignInError }',
);

// The module web/src actually imports. Imported (not extracted) so this asserts
// real runtime behavior of the shipping code path, types included. A file URL
// is required: a bare Windows path is rejected as an ESM URL scheme.
const live = await import(
  pathToFileURL(path.join(ROOT, 'web/src/lib/signinError.ts')).href
);
const describeLiveSignInError = live.describeSignInError;

const M1 = 'Google did not return a refresh token. If you have authorized Fluence before, revoke its access at myaccount.google.com/permissions and try signing in again.';

// The actionable no-refresh-token message is user-facing by design and must
// survive verbatim — including its own remedy URL.
test('M1 actionable message survives verbatim, URL intact', () => {
  assert.equal(describeSignInError('Error: ' + M1), M1);
  assert.equal(describeSignInError(M1, 'Sign-out'), M1);
});

// reqwest-style errors embed the request URL. It must never reach a toast.
test('request URL errors are mapped, URL never rendered', () => {
  const raw =
    'account lookup failed: error sending request for url (https://oauth2.googleapis.com/token): connection closed';
  const out = describeSignInError(raw);
  assert.doesNotMatch(out, /https?:\/\//);
  assert.doesNotMatch(out, /oauth2\.googleapis/);
  assert.match(out, /Google account/);
});

// Token-endpoint failures can carry provider bodies.
test('provider JSON bodies are mapped, never rendered', () => {
  const raw = 'token endpoint HTTP 503: {"error": {"code": 503, "message": "backend_error_x9"}}';
  const out = describeSignInError(raw);
  assert.doesNotMatch(out, /backend_error_x9/);
  assert.doesNotMatch(out, /[{}]/);
});

// Markup is cut the same way.
test('HTML error pages are mapped', () => {
  const out = describeSignInError('<html><head><title>503 Service Unavailable</title></head></html>');
  assert.doesNotMatch(out, /<html/);
});

// Short plain internal errors stay visible with the action prefix.
test('short plain errors keep the action label', () => {
  assert.equal(describeSignInError('sign-in did not produce an access token'), 'Sign-in failed: sign-in did not produce an access token');
  assert.equal(describeSignInError('vault locked', 'Sign-out'), 'Sign-out failed: vault locked');
});

// Long diagnostics are dumps, not messages.
test('over-long diagnostics collapse to the generic fallback', () => {
  const out = describeSignInError('x'.repeat(500));
  assert.equal(out, 'Sign-in failed. Please try again.');
});

// Empty and odd inputs never crash and never render raw.
test('empty and non-string inputs are safe', () => {
  assert.equal(describeSignInError(''), 'Sign-in failed. Please try again.');
  assert.equal(describeSignInError(null), 'Sign-in failed. Please try again.');
  assert.equal(describeSignInError(undefined, 'Sign-out'), 'Sign-out failed. Please try again.');
});

// The default action is Sign-in when the caller passes nothing.
test('action defaults to Sign-in', () => {
  assert.match(describeSignInError('some unknown short failure'), /^Sign-in failed/);
});

// ---------------------------------------------------------------------------
// LIVE SURFACE (web/src) — the code that actually ships. Same contract, run
// against web/src/lib/signinError.ts instead of the dead vanilla copy.
// ---------------------------------------------------------------------------

test('LIVE: M1 actionable message survives verbatim, URL intact', () => {
  assert.equal(describeLiveSignInError('Error: ' + M1), M1);
  assert.equal(describeLiveSignInError(M1, 'Sign-out'), M1);
});

test('LIVE: request URL errors are mapped, URL never rendered', () => {
  const raw =
    'account lookup failed: error sending request for url (https://oauth2.googleapis.com/token): connection closed';
  const out = describeLiveSignInError(raw);
  assert.doesNotMatch(out, /https?:\/\//);
  assert.doesNotMatch(out, /oauth2\.googleapis/);
  assert.match(out, /Google account/);
});

test('LIVE: provider JSON bodies are mapped, never rendered', () => {
  const raw =
    'token endpoint HTTP 503: {"error": {"code": 503, "message": "backend_error_x9"}}';
  const out = describeLiveSignInError(raw);
  assert.doesNotMatch(out, /backend_error_x9/);
  assert.doesNotMatch(out, /[{}]/);
});

test('LIVE: HTML error pages are mapped', () => {
  const out = describeLiveSignInError(
    '<html><head><title>503 Service Unavailable</title></head></html>',
  );
  assert.doesNotMatch(out, /<html/);
});

test('LIVE: short plain errors keep the action label', () => {
  assert.equal(
    describeLiveSignInError('sign-in did not produce an access token'),
    'Sign-in failed: sign-in did not produce an access token',
  );
  assert.equal(
    describeLiveSignInError('vault locked', 'Sign-out'),
    'Sign-out failed: vault locked',
  );
  assert.equal(
    describeLiveSignInError('scheduler latch busy', 'Sync'),
    'Sync failed: scheduler latch busy',
  );
  assert.equal(
    describeLiveSignInError('y'.repeat(200), 'Sync'),
    'Sync failed. Please try again.',
  );
});

test('LIVE: over-long diagnostics collapse to the generic fallback', () => {
  const out = describeLiveSignInError('x'.repeat(500));
  assert.equal(out, 'Sign-in failed. Please try again.');
});

test('LIVE: empty and non-string inputs are safe', () => {
  assert.equal(describeLiveSignInError(''), 'Sign-in failed. Please try again.');
  assert.equal(describeLiveSignInError(null), 'Sign-in failed. Please try again.');
  assert.equal(
    describeLiveSignInError(undefined, 'Sign-out'),
    'Sign-out failed. Please try again.',
  );
});

test('LIVE: action defaults to Sign-in', () => {
  assert.match(
    describeLiveSignInError('some unknown short failure'),
    /^Sign-in failed/,
  );
});

// The exact strings the Rust AuthError variants named in the audit produce.
test('LIVE: the real AuthError shapes never render verbatim', () => {
  const raws = [
    'network error: error sending request for url (https://oauth2.googleapis.com/token)',
    'token endpoint HTTP 500: {"error":"invalid_grant","error_description":"Bad Request"}',
  ];
  for (const raw of raws) {
    const out = describeLiveSignInError(raw);
    assert.doesNotMatch(out, /oauth2\.googleapis/);
    assert.doesNotMatch(out, /invalid_grant/);
    assert.doesNotMatch(out, /[{}]/);
    assert.doesNotMatch(out, /https?:\/\//);
  }
});

// The original live bug, pinned at the source so the wiring cannot revert.
test('LIVE: SyncPage routes its error toasts through the mapper, not String(err)', () => {
  const page = readFileSync(path.join(ROOT, 'web/src/routes/SyncPage.tsx'), 'utf8');
  assert.match(
    page,
    /import \{ describeSignInError \} from '@\/lib\/signinError'/,
    'SyncPage no longer imports the live sanitizer',
  );
  assert.match(page, /toast\(describeSignInError\(err\), 'error'\)/);
  assert.match(page, /toast\(describeSignInError\(err, 'Sign-out'\), 'error'\)/);
  assert.match(page, /toast\(describeSignInError\(err, 'Sync'\), 'error'\)/);
  // The exact raw concatenations the audit found.
  assert.doesNotMatch(page, /Sign-in failed: ' \+ String\(err\)/);
  assert.doesNotMatch(page, /Sign-out failed: ' \+ String\(err\)/);
  assert.doesNotMatch(page, /Failed to start sync: ' \+ String\(err\)/);
  // last_error is component state; parking the raw provider string there
  // re-opens the leak for whatever renders it next.
  assert.doesNotMatch(page, /last_error: String\(err\)/);
});
