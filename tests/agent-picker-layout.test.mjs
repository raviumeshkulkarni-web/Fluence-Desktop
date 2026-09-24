import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import { extractFunction, evalFunctions } from './extract-js-function.mjs';

const ROOT = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const overlayJsPath = path.join(ROOT, 'src/js/overlay.js');
const overlayCssPath = path.join(ROOT, 'src/css/overlay.css');
const computeAgentPickerExpansion = evalFunctions(
  [extractFunction(overlayJsPath, 'computeAgentPickerExpansion')],
  'computeAgentPickerExpansion',
);

test('agent picker expands by the measured card and its gap', () => {
  assert.equal(computeAgentPickerExpansion(144), 152);
  assert.equal(computeAgentPickerExpansion(20), 28);
  assert.equal(computeAgentPickerExpansion(Number.NaN), 8);
});

test('only visible picker controls capture pointer input', () => {
  const css = readFileSync(overlayCssPath, 'utf8');
  assert.match(css, /\.agent-picker\s*\{[^}]*pointer-events:\s*none;/s);
  assert.match(css, /\.agent-strip\s*\{[^}]*pointer-events:\s*auto;/s);
  assert.match(css, /\.agent-card\s*\{[^}]*pointer-events:\s*auto;/s);
  assert.match(css, /\.overlay-root\.active\s*\{[^}]*pointer-events:\s*auto;/s);
});

test('picker stays above the app pill and maintains zero-shift invariant', () => {
  const css = readFileSync(overlayCssPath, 'utf8');
  assert.match(css, /\.agent-strip\s*\{[^}]*top:\s*0;[^}]*bottom:\s*auto;/s);
  assert.match(css, /body\s*\{[^}]*padding-top:\s*calc\(46px \+ var\(--agent-picker-extra\)\);/s);
  assert.match(css, /\.app-pill\s*\{[^}]*top:\s*calc\(8px \+ var\(--agent-picker-extra\)\);/s);
  assert.match(css, /body\.agent-picker-expanded \.agent-strip\s*\{[^}]*top:\s*calc\(var\(--agent-picker-card-height\) \+ 8px\);/s);
});

test('agent list uses the overlay scrollbar theme and remains capped', () => {
  const css = readFileSync(overlayCssPath, 'utf8');
  assert.match(css, /\.agent-list\s*\{[^}]*max-height:\s*140px;[^}]*overflow-x:\s*hidden;[^}]*overflow-y:\s*auto;[^}]*scrollbar-color:\s*rgba\(11, 214, 227, 0\.5\) transparent;/s);
  assert.match(css, /\.agent-list::-webkit-scrollbar-thumb\s*\{[^}]*background:\s*rgba\(11, 214, 227, 0\.48\);/s);
});

test('agent picker card keeps one canonical width across tiers', () => {
  const css = readFileSync(overlayCssPath, 'utf8');
  // 220px in every tier, harmonizing with 44px bubble, 118px pill, and 320px island.
  assert.match(css, /\.agent-picker\s*\{[^}]*width:\s*220px;/s);
  const rust = readFileSync(path.join(ROOT, 'src-tauri/src/overlay.rs'), 'utf8');
  // Window fits it: 220 + 2x20 margin, widening compact/bubble transiently.
  assert.match(rust, /AGENT_PICKER_WINDOW_WIDTH:\s*f64\s*=\s*260\.0;/);
  assert.match(rust, /base_width\.max\(AGENT_PICKER_WINDOW_WIDTH\)/);
});

test('window resizes carry picker visibility so small tiers fit the card', () => {
  const js = readFileSync(overlayJsPath, 'utf8');
  assert.match(js, /set_agent_picker_height', \{ extraHeight, pickerVisible \}/);
});

test('picker motion glides on the expo-out curve and collapses under reduced motion', () => {
  const css = readFileSync(overlayCssPath, 'utf8');
  assert.match(css, /@keyframes agent-card-in\s*\{/);
  assert.match(css, /@keyframes agent-strip-in\s*\{/);
  assert.match(css, /prefers-reduced-motion:\s*reduce[\s\S]*?\.agent-card\s*\{[^}]*transition-duration:\s*1ms;/);
});

test('startup state does not activate the hidden overlay before the native show', () => {
  const setup = extractFunction(overlayJsPath, 'setupEventListeners');
  const setState = extractFunction(overlayJsPath, 'setState');
  assert.match(setup, /setState\('recording', false\)/);
  assert.match(setup, /setState\('agent', false\)/);
  assert.match(setState, /function setState\(state, updateVisibility = true\)/);
  assert.match(setState, /if \(overlayRoot && updateVisibility\)/);
});

test('exit defers picker resizing until after the visible exit animation', () => {
  const fade = extractFunction(overlayJsPath, 'fadeAndHide');
  assert.match(fade, /hideAgentPicker\(false\)/);
  assert.match(fade, /await new Promise\(r => setTimeout\(r, 200\)\)/);
  assert.ok(fade.indexOf('hideAgentPicker(false)') < fade.indexOf("setTimeout(r, 200)"));
});

test('hiding the overlay always restores its base bounds', () => {
  const rust = readFileSync(path.join(ROOT, 'src-tauri/src/overlay.rs'), 'utf8');
  const hideStart = rust.indexOf('pub fn hide_overlay');
  const hideEnd = rust.indexOf('\n#[tauri::command]', hideStart);
  assert.ok(hideStart >= 0 && hideEnd > hideStart);
  assert.match(rust.slice(hideStart, hideEnd), /place_overlay_window\(/);
});
