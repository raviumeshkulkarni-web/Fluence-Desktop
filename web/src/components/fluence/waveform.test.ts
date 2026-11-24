// The waveform animation pick is a frontend contract with a Rust-typed
// settings field: unknown values MUST fall back to "waves" (the shipped
// look), the option list must cover every valid value, and copy must stay
// em-dash-free like all release-facing strings. Colors are not part of
// this contract: the canvas keeps the shipped amethyst/cyan pairing and
// the General-tab accent never touches the waveform.

import test from 'node:test';
import assert from 'node:assert/strict';
import { drawWaveformFrame, normalizeWaveformStyle, WAVEFORM_STYLE_OPTIONS } from './waveform.ts';

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

test('drawWaveformFrame runs with both transcribe and agent mode and uses correct mode colors', () => {
  const prevWindow = globalThis.window;
  (globalThis as unknown as { window: unknown }).window = { devicePixelRatio: 1 };
  try {
    const recordedStops: { offset: number; color: string }[] = [];
    const ctx = {
      setTransform: () => {},
      scale: () => {},
      clearRect: () => {},
      beginPath: () => {},
      moveTo: () => {},
      lineTo: () => {},
      stroke: () => {},
      set strokeStyle(_val: unknown) {},
      set fillStyle(_val: unknown) {},
      fillRect: () => {},
      createLinearGradient: () => ({
        addColorStop: (offset: number, color: string) => {
          recordedStops.push({ offset, color });
        },
      }),
    };
    const canvas = {
      offsetWidth: 100,
      offsetHeight: 40,
      width: 0,
      height: 0,
      getContext: () => ctx,
    } as unknown as HTMLCanvasElement;

    // 1. Transcribe mode (waves): primary is #8B45D8 (rgba(139,69,216,...)), forefront #F1EAF5 (rgba(241,234,245,...))
    recordedStops.length = 0;
    drawWaveformFrame(canvas, 'waves', 0.5, 0.5, 'full', 'transcribe');
    assert.ok(recordedStops.some((s) => s.color.startsWith('rgba(139,69,216,')), 'transcribe wave must use #8B45D8 (139,69,216)');
    assert.ok(recordedStops.some((s) => s.color.startsWith('rgba(241,234,245,')), 'transcribe wave must use forefront #F1EAF5 (241,234,245)');
    assert.ok(!recordedStops.some((s) => s.color.includes('11,214,227')), 'transcribe wave must NOT contain agent cyan');

    // 2. Agent mode (waves): primary is #0BD6E3 (rgba(11,214,227,...)), forefront #E6FBFC (rgba(230,251,252,...))
    recordedStops.length = 0;
    drawWaveformFrame(canvas, 'waves', 0.5, 0.5, 'full', 'agent');
    assert.ok(recordedStops.some((s) => s.color.startsWith('rgba(11,214,227,')), 'agent wave must use #0BD6E3 (11,214,227)');
    assert.ok(recordedStops.some((s) => s.color.startsWith('rgba(230,251,252,')), 'agent wave must use forefront #E6FBFC (230,251,252)');
    assert.ok(!recordedStops.some((s) => s.color.includes('139,69,216')), 'agent wave must NOT contain transcribe purple');

    // 3. Transcribe mode (bars): primary is #8B45D8, forefront #F1EAF5
    recordedStops.length = 0;
    drawWaveformFrame(canvas, 'bars', 0.5, 0.5, 'compact', 'transcribe');
    assert.ok(recordedStops.some((s) => s.color.startsWith('rgba(139,69,216,')), 'transcribe bars must use #8B45D8');
    assert.ok(recordedStops.some((s) => s.color.startsWith('rgba(241,234,245,')), 'transcribe bars must use #F1EAF5');

    // 4. Agent mode (bars): primary is #0BD6E3, forefront #E6FBFC
    recordedStops.length = 0;
    drawWaveformFrame(canvas, 'bars', 0.5, 0.5, 'compact', 'agent');
    assert.ok(recordedStops.some((s) => s.color.startsWith('rgba(11,214,227,')), 'agent bars must use #0BD6E3');
    assert.ok(recordedStops.some((s) => s.color.startsWith('rgba(230,251,252,')), 'agent bars must use #E6FBFC');

    // 5. Default mode when omitted defaults to transcribe (#8B45D8)
    recordedStops.length = 0;
    drawWaveformFrame(canvas, 'waves', 0.5, 0.5, 'full');
    assert.ok(recordedStops.some((s) => s.color.startsWith('rgba(139,69,216,')), 'omitted mode must default to transcribe');

    // 6. Unknown mode falls back gracefully to transcribe (#8B45D8) without throwing
    recordedStops.length = 0;
    drawWaveformFrame(canvas, 'waves', 0.5, 0.5, 'bubble', 'unknown' as unknown as 'transcribe');
    assert.ok(recordedStops.some((s) => s.color.startsWith('rgba(139,69,216,')), 'unknown mode must fall back to transcribe');
  } finally {
    (globalThis as unknown as { window: unknown }).window = prevWindow;
  }
});
