// Polish skip-reason mapping tests.
// Extracts the REAL `mapPolishReason` from src/js/overlay.js and asserts the
// user-facing labels for every backend reason code plus unknown/null input.
// Run: node --test tests/polish-reason-mapping.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import { extractFunction, evalFunctions } from './extract-js-function.mjs';

const ROOT = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const overlayPath = path.join(ROOT, 'src/js/overlay.js');
const runSttFlowSource = extractFunction(overlayPath, 'runSttFlow');
const mapPolishReason = evalFunctions(
  [extractFunction(path.join(ROOT, 'src/js/overlay.js'), 'mapPolishReason')],
  'mapPolishReason',
);

test('known reason codes map to short labels', () => {
  assert.equal(mapPolishReason('missing_key'), 'no key set');
  assert.equal(mapPolishReason('rate_limited'), 'rate limited');
  assert.equal(mapPolishReason('network_error'), 'network error');
  assert.equal(mapPolishReason('rejected'), 'AI output rejected');
  assert.equal(mapPolishReason('ai_error'), 'AI error');
});

test('unknown, missing, and empty reasons fall back to AI error', () => {
  assert.equal(mapPolishReason('something_new'), 'AI error');
  assert.equal(mapPolishReason(undefined), 'AI error');
  assert.equal(mapPolishReason(null), 'AI error');
  assert.equal(mapPolishReason(''), 'AI error');
});

test('transcription success never exposes AI-skipped fallback copy', () => {
  assert.doesNotMatch(runSttFlowSource, /polishFallback|polishReason|AI skipped|mapPolishReason/);
  assert.match(runSttFlowSource, /Inserted \(standard mode\)/);
  assert.match(runSttFlowSource, /'Inserted'/);
});
