import test from 'node:test';
import assert from 'node:assert/strict';

import { describeError } from './errors.ts';

const FALLBACK = 'Something went wrong. Please try again.';
const AUTH = 'The API key was rejected. Check the key and try again.';
const RATE = 'The provider is limiting requests right now. Try again in a moment.';
const NETWORK =
  'Could not reach the server. Check your connection and the endpoint URL, then try again.';
const NOT_FOUND =
  'The provider could not find that endpoint or model. Check the URL and model name.';
const SERVER = 'The provider returned a server error. Try again shortly.';

test('auth status words and exact codes map to the key message', () => {
  for (const raw of [
    '401 Unauthorized',
    'HTTP 403: forbidden',
    'invalid api key',
    'Error: API key was rejected (401)',
  ]) {
    assert.equal(describeError(raw), AUTH, raw);
  }
});

test('rate-limit messages map', () => {
  for (const raw of ['429 Too Many Requests', 'rate limit exceeded', 'quota exhausted']) {
    assert.equal(describeError(raw), RATE, raw);
  }
});

test('network failures map', () => {
  for (const raw of [
    'connection refused',
    'dns lookup failed',
    'request timed out',
    'econnreset',
  ]) {
    assert.equal(describeError(raw), NETWORK, raw);
  }
});

test('not-found and server errors map', () => {
  assert.equal(describeError('HTTP 404'), NOT_FOUND);
  assert.equal(describeError('model not found'), NOT_FOUND);
  assert.equal(describeError('HTTP 503'), SERVER);
  assert.equal(describeError('internal server error'), SERVER);
});

test('local storage and OS failures map', () => {
  assert.equal(
    describeError('failed to access Windows Credential Manager'),
    'Could not access Windows Credential Manager. Try again.',
  );
  assert.equal(describeError('database is locked'), 'Could not open the local database. Try again.');
  assert.equal(
    describeError('Access is denied.'),
    'Windows blocked that action. Check permissions and try again.',
  );
  assert.equal(
    describeError('The system cannot find the file specified. (os error 2)'),
    'A required file is missing. Repair or reinstall Fluence, then try again.',
  );
});

test('paths, URLs, registry keys, and model filenames never pass through', () => {
  for (const raw of [
    String.raw`Cannot open C:\Users\me\AppData\Local\Fluence\settings.json`,
    String.raw`copy failed: \\server\share\model.bin`,
    'open ./web/src/dist/assets/index.js',
    String.raw`HKEY_CURRENT_USER\Software\Fluence`,
    'Model file ggml-base.en.bin is missing',
    'cannot open /var/lib/fluence/cache/',
    'request failed for https://api.openai.com/v1',
    "thread 'main' panicked at src/main.rs:42",
    'unexpected end of JSON input',
  ]) {
    assert.equal(describeError(raw), FALLBACK, raw);
  }
});

test('structured dumps and overlong diagnostics fall back', () => {
  assert.equal(describeError('{"error":"backend_error_x9"}'), FALLBACK);
  assert.equal(describeError('a'.repeat(161)), FALLBACK);
});

test('short plain user messages survive, with the Error prefix stripped', () => {
  assert.equal(describeError('The model is not available'), 'The model is not available');
  assert.equal(describeError('Copied 3 files'), 'Copied 3 files');
  assert.equal(describeError('version 1.2.3 is ready'), 'version 1.2.3 is ready');
  assert.equal(describeError('Use and/or in the filter'), 'Use and/or in the filter');
  assert.equal(describeError('Error: Could not write the transcript'), 'Could not write the transcript');
});

test('numbers that merely contain status digits are not status codes', () => {
  assert.equal(describeError('Processed 5123 bytes'), 'Processed 5123 bytes');
  assert.equal(describeError('Wrote 14012 rows'), 'Wrote 14012 rows');
  assert.equal(describeError('Took 4012 ms'), 'Took 4012 ms');
});

test('empty and non-string errors use the requested fallback', () => {
  assert.equal(describeError(''), FALLBACK);
  assert.equal(describeError(null), FALLBACK);
  assert.equal(describeError('', 'Custom fallback'), 'Custom fallback');
  assert.equal(describeError(new Error('HTTP 500')), SERVER);
});
