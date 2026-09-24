import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

const ROOT = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const css = readFileSync(path.join(ROOT, 'web/src/app.css'), 'utf8');
const preview = readFileSync(path.join(ROOT, 'web/src/components/fluence/OverlayPreview.tsx'), 'utf8');

test('bubble preview hosts reserve their real tier height', () => {
  assert.match(css, /\.overlay-preview-full\s*\{[^}]*width:\s*320px;[^}]*height:\s*96px;/s);
  assert.match(css, /\.overlay-preview-compact\s*\{[^}]*width:\s*118px;[^}]*height:\s*36px;/s);
  assert.match(css, /\.overlay-preview-bubble\s*\{[^}]*width:\s*44px;[^}]*height:\s*44px;/s);
});

test('bubble preview hosts reserve the app pill without overlapping rows', () => {
  assert.match(css, /\.overlay-preview-full\.has-pill\s*\{[^}]*height:\s*142px;/s);
  assert.match(css, /\.overlay-preview-compact\.has-pill\s*\{[^}]*height:\s*82px;/s);
  assert.match(css, /\.overlay-preview-bubble\.has-pill\s*\{[^}]*height:\s*90px;/s);
  assert.match(preview, /overlay-preview-\$\{tier\}.*has-pill/s);
});

test('preview frame defines the body-level picker vars used by the app pill', () => {
  assert.match(preview, /\.ovpv-frame\s*\{[^}]*--agent-picker-extra:\s*0px;/s);
  assert.match(preview, /\.ovpv-frame\s*\{[^}]*--agent-picker-card-height:\s*0px;/s);
});

test('selected agent rows keep the card edge unclipped', () => {
  assert.match(css, /\.settings-card-selected\s*\{[^}]*border-color:/s);
  assert.doesNotMatch(css, /\.selection-row\.selected\s*\{[^}]*box-shadow:/s);
});
