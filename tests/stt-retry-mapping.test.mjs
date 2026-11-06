// STT failure mapping: permanent config/auth/model/input failures must not
// offer Retry (the retained audio would fail again); transient provider,
// network, and local-sidecar failures must. Extracts the REAL mapping from
// src/js/overlay.js. Run: node --test tests/stt-retry-mapping.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import { extractFunction, evalFunctions } from './extract-js-function.mjs';

const ROOT = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const overlayPath = path.join(ROOT, 'src/js/overlay.js');
const mapSttErrorToStatus = evalFunctions(
  [extractFunction(overlayPath, 'mapSttErrorToStatus')],
  'mapSttErrorToStatus',
);
const runSttFlowSource = extractFunction(overlayPath, 'runSttFlow');
const stopAndTranscribeSource = extractFunction(overlayPath, 'stopAndTranscribe');

function check(err, label, retryable) {
  assert.deepEqual(mapSttErrorToStatus(err), { label, retryable }, `for ${JSON.stringify(err)}`);
}

test('missing STT key is non-retryable and names the fix', () => {
  check(
    'No API key found for groq. Please configure it in Providers settings.',
    'Missing STT key',
    false,
  );
});

test('URL and recording-limit failures are non-retryable', () => {
  check("Invalid URL 'not-a-url': relative URL without a base", 'Check STT URL', false);
  check(
    'HTTP is only allowed for localhost development (got 192.168.1.50). Use HTTPS for remote servers.',
    'Check STT URL',
    false,
  );
  check(
    'Recording too long (400000 samples, 10.5 minutes). Maximum is 10 minutes for offline mode.',
    'Recording too long',
    false,
  );
  check('No captured audio is available to retry', 'Nothing to retry', false);
});

test('offline setup failures are non-retryable', () => {
  check(
    'Offline transcription error: Offline transcription engine is not installed. Please download it in Settings.',
    'Offline setup missing',
    false,
  );
  check(
    'Offline transcription error: SenseVoice model files are not installed. Please download them in Settings.',
    'Offline setup missing',
    false,
  );
  check(
    "Offline transcription error: Moonshine v2 model file 'encoder.onnx' is missing. Please download the model in Settings.",
    'Offline setup missing',
    false,
  );
  check(
    'Offline transcription error: Binary integrity check failed: checksum mismatch. Please re-download the model in Settings.',
    'Offline setup missing',
    false,
  );
  check(
    'Offline transcription error: The local transcription sidecar on port 1234 answered a connection but not the start-up handshake.',
    'Offline setup missing',
    false,
  );
});

test('provider auth and validation failures are non-retryable', () => {
  check('API error 401 Unauthorized: invalid key', 'STT auth failed', false);
  check('API error 403 Forbidden', 'STT auth failed', false);
  check('API error 400 Bad Request: unsupported audio', 'Check STT model', false);
  check('API error 404 Not Found: model missing', 'Check STT model', false);
  check('API error 422 Unprocessable Entity', 'Check STT model', false);
  check('API error 400: model is not installed on this server', 'Check STT model', false);
});

test('transient provider failures stay retryable', () => {
  check('API error 408 Request Timeout', 'Network error', true);
  check('API error 429 Too Many Requests', 'Rate limited. Retry', true);
  check('API error 500 Internal Server Error', 'STT unavailable', true);
  check('API error 502 Bad Gateway', 'STT unavailable', true);
  check('API error 503 Service Unavailable: model overloaded', 'STT unavailable', true);
  check('API error 504 Gateway Timeout', 'STT unavailable', true);
});

test('network and local-sidecar failures stay retryable', () => {
  check(
    'Network error: error sending request for url (https://api.groq.com/openai/v1/audio/transcriptions)',
    'Network error',
    true,
  );
  check('Network error: connection timed out', 'Network error', true);
  check(
    "Offline transcription error: Local ASR server is busy (connection refused at the sidecar's capacity limit); short wait and try again",
    'Network error',
    true,
  );
  check(
    'Offline transcription error: Timed out waiting for local ASR server response',
    'Network error',
    true,
  );
  check(
    'Offline transcription error: Failed to connect to local ASR server: connection refused',
    'Network error',
    true,
  );
  check('Offline transcription error: Empty response from ASR server', 'STT response failed', true);
  check('JSON parse error: expected value at line 1 column 1', 'STT response failed', true);
});

test('unknown failures stay buttonless rather than offering a dead retry', () => {
  check('something completely unexpected', 'Transcription failed', false);
  check('', 'Transcription failed', false);
});

test('both STT failure paths use the mapping before showing Retry', () => {
  for (const source of [runSttFlowSource, stopAndTranscribeSource]) {
    assert.match(source, /mapSttErrorToStatus\(err\)/);
    assert.match(source, /if \(retryable\) showRetry\(\)/);
    assert.doesNotMatch(source, /setStatusMessage\('Transcription failed'\);\s*showRetry\(\)/);
  }
});
