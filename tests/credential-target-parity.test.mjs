// FIX-02 tests: frontend/backend credential-target parity.
// Extracts the REAL canonicalization functions — `canonicalPresetSlug` /
// `keyTarget` from web/src/ipc/providers.ts (TypeScript, transpiled with the
// repo's own typescript) and `canonicalPresetSlug` / `credentialTarget` from
// src/js/settings.js — and asserts they agree with each other and with the
// backend contract (same expectations as the Rust tests in credentials.rs).
// Run: node --test tests/credential-target-parity.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import { createRequire } from 'node:module';
import { extractFunction, evalFunctions } from './extract-js-function.mjs';

const ROOT = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const require = createRequire(import.meta.url);
const ts = require('../web/node_modules/typescript');

function loadTsFunctions(tsPath, names) {
  // NB: extractFunction starts at the `function` keyword, dropping the
  // leading `export ` — re-add it so the transpile emits real exports.
  const sources = names.map((n) => `export ${extractFunction(tsPath, n)}`);
  const { outputText } = ts.transpileModule(sources.join('\n'), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2020 },
  });
  const code = outputText.replace(/^"use strict";/, '');
  const module = { exports: {} };
  new Function('module', 'exports', code)(module, module.exports);
  return module.exports;
}

const providersPath = path.join(ROOT, 'web/src/ipc/providers.ts');
const settingsPath = path.join(ROOT, 'src/js/settings.js');

const tsFns = loadTsFunctions(providersPath, ['canonicalPresetSlug', 'keyTarget']);
const jsFns = evalFunctions(
  [
    extractFunction(settingsPath, 'canonicalPresetSlug'),
    extractFunction(settingsPath, 'credentialTarget'),
  ],
  '{ canonicalPresetSlug, credentialTarget }',
);

// [preset, expectedSlug] — mirrors the Rust `canonical_slug_*` tests.
const SLUG_CASES = [
  ['groq', 'groq'],
  ['openai', 'openai'],
  ['mistral', 'mistral'],
  ['custom', 'custom'],
  ['Local Offline', 'local_offline'],
  ['My Provider', 'my_provider'],
  ['deep-infra', 'deep_infra'],
  ['deep_infra', 'deep_infra'],
  ['Deep Infra', 'deep_infra'],
  ['GROQ', 'groq'],
  ['groq-key', 'groq_key'],
  ['llama-cpp', 'llama_cpp'],
  ['a/b', 'a_b'],
  ['C:\\keys', 'c__keys'],
  ['UPPER CASE', 'upper_case'],
  ['   ', '___'],
];

test('TS keyTarget and JS credentialTarget agree on every case', () => {
  for (const [preset, slug] of SLUG_CASES) {
    assert.equal(tsFns.keyTarget('llm', preset), `Fluence/LLM_ApiKey/${slug}`, `TS llm ${preset}`);
    assert.equal(tsFns.keyTarget('stt', preset), `Fluence/STT_ApiKey/${slug}`, `TS stt ${preset}`);
    assert.equal(jsFns.credentialTarget('llm', preset), `Fluence/LLM_ApiKey/${slug}`, `JS llm ${preset}`);
    assert.equal(jsFns.credentialTarget('stt', preset), `Fluence/STT_ApiKey/${slug}`, `JS stt ${preset}`);
  }
});

test('collision case shares one slot on both sides', () => {
  assert.equal(tsFns.canonicalPresetSlug('my-provider'), 'my_provider');
  assert.equal(jsFns.canonicalPresetSlug('my-provider'), 'my_provider');
  assert.equal(tsFns.keyTarget('llm', 'my-provider'), tsFns.keyTarget('llm', 'my_provider'));
  assert.equal(
    jsFns.credentialTarget('llm', 'my-provider'),
    jsFns.credentialTarget('llm', 'my_provider'),
  );
});

test('slugs never carry path or traversal characters', () => {
  for (const preset of ['a/b', '../x', '..\\..\\secret', 'C:\\keys', '/abs', 'a:b', 'a.b']) {
    for (const slug of [tsFns.canonicalPresetSlug(preset), jsFns.canonicalPresetSlug(preset)]) {
      assert.match(slug, /^[a-z0-9_]*$/, `slug for ${JSON.stringify(preset)}: ${slug}`);
      assert.ok(!slug.includes('..'), `no traversal in ${slug}`);
    }
  }
});

test('built-in presets resolve to their historical slots', () => {
  assert.equal(tsFns.keyTarget('llm', 'groq'), 'Fluence/LLM_ApiKey/groq');
  assert.equal(jsFns.credentialTarget('stt', 'Local Offline'), 'Fluence/STT_ApiKey/local_offline');
});
