// The waveform animation pick is a frontend contract with a Rust-typed
// settings field: unknown values MUST fall back to "waves" (the shipped
// look), the option list must cover every valid value, and copy must stay
// em-dash-free like all release-facing strings. Colors are not part of
// this contract: the canvas keeps the shipped amethyst/cyan pairing and
// the General-tab accent never touches the waveform.

import test from 'node:test';
import assert from 'node:assert/strict';
import { normalizeWaveformStyle, WAVEFORM_STYLE_OPTIONS } from './waveform.ts';

test('normalizeWaveformStyle falls back to waves for unknown values', () => {
  assert.equal(normalizeWaveformStyle('waves'), 'waves');
  assert.equal(normalizeWaveformStyle('bars'), 'bars');
  assert.equal(normalizeWaveformStyle('dots'), 'waves');
  assert.equal(normalizeWaveformStyle('spikes'), 'waves');
  assert.equal(normalizeWaveformStyle('helix'), 'waves');
  assert.equal(normalizeWaveformStyle('wash'), 'waves');
  assert.equal(normalizeWaveformStyle('orb'), 'waves');
  assert.equal(normalizeWaveformStyle('line'), 'waves');
  assert.equal(normalizeWaveformStyle(undefined), 'waves');
  assert.equal(normalizeWaveformStyle(null), 'waves');
  assert.equal(normalizeWaveformStyle(''), 'waves');
  assert.equal(normalizeWaveformStyle('siri'), 'waves');
  assert.equal(normalizeWaveformStyle(42), 'waves');
});

test('the option list covers every valid style exactly once', () => {
  const values = WAVEFORM_STYLE_OPTIONS.map((o) => o.value);
  assert.equal(values.length, 2);
  assert.equal(new Set(values).size, 2);
  for (const style of ['waves', 'bars'] as const) {
    assert.ok(values.includes(style), `${style} is missing from the picker`);
  }
  assert.equal(values[0], 'waves', 'the shipped look must be the first (default) row');
});

test('every option has a title and a plain description', () => {
  for (const option of WAVEFORM_STYLE_OPTIONS) {
    assert.ok(option.title.trim().length > 0, `${option.value} needs a title`);
    assert.ok(option.desc.trim().length >= 12, `${option.value} needs a real description`);
    assert.ok(!option.desc.includes('\u2014'), `${option.value} description must be em-dash-free`);
  }
});
