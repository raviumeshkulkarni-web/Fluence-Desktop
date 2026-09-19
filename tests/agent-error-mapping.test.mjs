// FIX-03 tests: overlay agent-error mapping.
// Extracts the REAL `mapAgentErrorToStatus` from src/js/overlay.js and asserts
// the required mapping table: config/validation/authorization/input-limit
// failures are non-retryable with clear labels; transient failures stay
// retryable. Run: node --test tests/agent-error-mapping.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import { extractFunction, evalFunctions } from './extract-js-function.mjs';

const ROOT = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const mapAgentErrorToStatus = evalFunctions(
  [extractFunction(path.join(ROOT, 'src/js/overlay.js'), 'mapAgentErrorToStatus')],
  'mapAgentErrorToStatus',
);

function check(err, label, retryable) {
  assert.deepEqual(mapAgentErrorToStatus(err), { label, retryable }, `for ${JSON.stringify(err)}`);
}

test('missing API key is non-retryable with actionable label', () => {
  check(
    "Missing API key for LLM provider 'groq'. Open Settings → Providers → LLM → Save key.",
    'Missing LLM key',
    false,
  );
});

test('invalid credential target is non-retryable', () => {
  check('Access denied: unknown credential target', 'Key config error', false);
  check('Invalid credential target', 'Key config error', false);
});

test('raw credential-store errors never surface as retryable', () => {
  check('CredReadW failed: Element not found. (os error 1168)', 'Key storage error', false);
  check('Credential Manager not supported on this platform', 'Key storage error', false);
});

test('unauthorized window is a non-retryable permission error', () => {
  check('Not allowed from this window', 'Not permitted here', false);
});

test('oversized input is a non-retryable input-limit error', () => {
  check('Text too long (100001 chars). Maximum is 100000 characters.', 'Input too long', false);
  check('Voice command exceeds maximum length', 'Input too long', false);
  check('Clipboard context exceeds maximum length', 'Input too long', false);
  check('Dictionary import too large (1 bytes). Maximum is 1000000 bytes (~1 MB).', 'Input too long', false);
});

test('invalid endpoint stays a non-retryable configuration error', () => {
  check("Invalid URL 'not-a-url': relative URL without a base", 'Check LLM URL', false);
  check(
    'HTTP is only allowed for localhost development (got 192.168.1.50). Use HTTPS for remote servers.',
    'Check LLM URL',
    false,
  );
});

test('provider auth failure stays non-retryable', () => {
  check('LLM auth failed (401). Check Providers → LLM API key and model.', 'LLM auth failed', false);
});

test('transient failures stay retryable', () => {
  check('LLM rate limited (429). Wait a moment and retry.', 'Rate limited. Retry', true);
  check('LLM provider unavailable (503). Retry shortly.', 'LLM unavailable', true);
  check('Network error: connection timed out', 'Network error', true);
  check('Action parse error: unknown action', 'Agent parse failed', true);
});

test('unknown errors fall back to generic retryable Agent failed', () => {
  check('something completely unexpected', 'Agent failed', true);
});
