// Regression guard for the LIVE sign-in/sign-out error surface (audit S7).
//
// The original guard (tests/signin-toast-sanitization.test.mjs) extracted
// `describeSignInError` from the legacy vanilla `src/js/settings.js` and passed
// — while the code that actually ships rendered the raw backend error. The
// vanilla file is not loaded: `tauri.conf.json` sets `frontendDist: "../src"`
// and serves `src/dist/index.html`, i.e. the Vite bundle built from web/src.
//
// So the mapping is now ALSO asserted against the module the React tree
// imports (`lib/signinError.ts`, exercised for real here), and the call sites
// are pinned so the wiring cannot silently revert to raw concatenation.
//
// Runs on Node's built-in runner via native type-stripping:
//   npm --prefix web run test

import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

import { describeSignInError } from './signinError.ts';

const SRC = dirname(fileURLToPath(import.meta.url));
const syncPage = readFileSync(join(SRC, '..', 'routes', 'SyncPage.tsx'), 'utf8');

const M1 =
  'Google did not return a refresh token. If you have authorized Fluence before, revoke its access at myaccount.google.com/permissions and try signing in again.';

// --- the mapping itself (ported from the vanilla suite) ---------------------

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

// The other half of the real `AuthError::Http` shape, without the
// "account lookup" prefix: the token endpoint URL plus the provider body.
test('bare token-endpoint failures are mapped, URL and body never rendered', () => {
  const raw =
    'Error: token exchange failed: network error: error sending request for url (https://oauth2.googleapis.com/token)';
  const out = describeSignInError(raw);
  assert.doesNotMatch(out, /oauth2\.googleapis/);
  assert.doesNotMatch(out, /https?:\/\//);
  assert.match(out, /Google sign-in did not complete/);
});

// Token-endpoint failures can carry provider bodies.
test('provider JSON bodies are mapped, never rendered', () => {
  const raw =
    'token endpoint HTTP 503: {"error": {"code": 503, "message": "backend_error_x9"}}';
  const out = describeSignInError(raw);
  assert.doesNotMatch(out, /backend_error_x9/);
  assert.doesNotMatch(out, /[{}]/);
});

// Markup is cut the same way.
test('HTML error pages are mapped', () => {
  const out = describeSignInError(
    '<html><head><title>503 Service Unavailable</title></head></html>',
  );
  assert.doesNotMatch(out, /<html/);
});

// Short plain internal errors stay visible with the action prefix.
test('short plain errors keep the action label', () => {
  assert.equal(
    describeSignInError('sign-in did not produce an access token'),
    'Sign-in failed: sign-in did not produce an access token',
  );
  assert.equal(
    describeSignInError('vault locked', 'Sign-out'),
    'Sign-out failed: vault locked',
  );
  // The sibling Sync Now toast on the same page must not claim to be a
  // sign-in failure when it was not.
  assert.equal(
    describeSignInError('scheduler latch busy', 'Sync'),
    'Sync failed: scheduler latch busy',
  );
  assert.equal(describeSignInError('y'.repeat(200), 'Sync'), 'Sync failed. Please try again.');
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
  assert.equal(
    describeSignInError(undefined, 'Sign-out'),
    'Sign-out failed. Please try again.',
  );
});

// The default action is Sign-in when the caller passes nothing.
test('action defaults to Sign-in', () => {
  assert.match(describeSignInError('some unknown short failure'), /^Sign-in failed/);
});

// The concrete strings the Rust side actually produces for the two leaking
// AuthError variants named in the audit. Pinned so a change to either Rust
// Display impl fails HERE rather than silently reaching a toast.
test('the live AuthError shapes never reach the caller verbatim', () => {
  const network =
    'network error: error sending request for url (https://oauth2.googleapis.com/token)';
  const http = 'token endpoint HTTP 500: {"error":"invalid_grant","error_description":"Bad Request"}';
  for (const raw of [network, http]) {
    const out = describeSignInError(raw);
    assert.doesNotMatch(out, /oauth2\.googleapis/);
    assert.doesNotMatch(out, /invalid_grant/);
    assert.doesNotMatch(out, /[{}]/);
  }
});

// --- the call sites ---------------------------------------------------------

test('SyncPage routes its provider-error toasts through the mapper', () => {
  assert.match(syncPage, /import \{ describeSignInError \} from '@\/lib\/signinError'/);
  assert.match(syncPage, /toast\(describeSignInError\(err\), 'error'\)/);
  assert.match(
    syncPage,
    /toast\(describeSignInError\(err, 'Sign-out'\), 'error'\)/,
  );
  assert.match(syncPage, /toast\(describeSignInError\(err, 'Sync'\), 'error'\)/);
});

// The regression itself, asserted as text so it cannot come back unnoticed:
// the raw concatenations the audit named must not exist anywhere on the page.
test('SyncPage no longer concatenates a raw error into a provider toast', () => {
  assert.doesNotMatch(syncPage, /Sign-in failed: ' \+ String\(err\)/);
  assert.doesNotMatch(syncPage, /Sign-out failed: ' \+ String\(err\)/);
  assert.doesNotMatch(syncPage, /Failed to start sync: ' \+ String\(err\)/);
});

// `last_error` is component state, so the raw string must not be parked there
// either — a later feature that renders it verbatim would re-open the leak.
test('SyncPage stores a sanitized last_error, not the raw error', () => {
  assert.match(
    syncPage,
    /last_error: describeSignInError\(err\)/,
  );
  assert.doesNotMatch(syncPage, /last_error: String\(err\)/);
});
