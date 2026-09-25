import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

const ROOT = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const css = readFileSync(path.join(ROOT, 'web/src/app.css'), 'utf8');
const preview = readFileSync(path.join(ROOT, 'web/src/components/fluence/OverlayPreview.tsx'), 'utf8');

test('bubble preview hosts reserve the scaled full tier and native smaller tiers', () => {
  assert.match(css, /\.bubble-option\s*\{[^}]*height:\s*150px;[^}]*min-height:\s*150px;/s);
  assert.match(css, /\.overlay-preview-full\s*\{[^}]*width:\s*240px;[^}]*height:\s*72px;/s);
  assert.match(css, /\.overlay-preview-compact\s*\{[^}]*width:\s*118px;[^}]*height:\s*36px;/s);
  assert.match(css, /\.overlay-preview-bubble\s*\{[^}]*width:\s*44px;[^}]*height:\s*44px;/s);
});

test('bubble preview hosts reserve the app pill without overlapping rows', () => {
  assert.match(css, /\.overlay-preview-full\.has-pill\s*\{[^}]*height:\s*107px;/s);
  assert.match(css, /\.overlay-preview-compact\.has-pill\s*\{[^}]*height:\s*82px;/s);
  assert.match(css, /\.overlay-preview-bubble\.has-pill\s*\{[^}]*height:\s*90px;/s);
  assert.match(preview, /overlay-preview-\$\{tier\}.*has-pill/s);
});

test('shared overlay rules never depend on a body-only var without a fallback', () => {
  // The settings preview inlines overlay.css into a shadow root where
  // `.ovpv-frame` stands in for `body` — so any `var(--agent-*)` used by a
  // selector that can match preview markup (.app-pill, .overlay-root, …)
  // must either carry a fallback or be declared on `.ovpv-frame`. A bare
  // body-scoped var invalidates the whole declaration (e.g. the app pill
  // fell back to `top: auto` and overlapped the island in settings).
  const overlayCss = readFileSync(path.join(ROOT, 'src/css/overlay.css'), 'utf8').replace(/\/\*[\s\S]*?\*\//g, '');
  const frameBlock = preview.match(/\.ovpv-frame\s*\{[^}]*\}/);
  assert.ok(frameBlock, '.ovpv-frame block missing from preview');
  const unresolvable = [];
  const ruleRe = /([^{}]+)\{([^{}]*)\}/g;
  let m;
  while ((m = ruleRe.exec(overlayCss)) !== null) {
    const selector = m[1].trim();
    if (/\bbody\b|\bhtml\b/.test(selector)) continue;
    const varRe = /var\(\s*(--agent-[a-z-]+)\s*(,\s*[^)]*)?\)/g;
    let v;
    while ((v = varRe.exec(m[2])) !== null) {
      const name = v[1];
      const hasFallback = Boolean(v[2]);
      const onFrame = new RegExp(`${name.replace(/-/g, '\\-')}\\s*:`).test(frameBlock[0]);
      if (!hasFallback && !onFrame) unresolvable.push(`${selector} -> ${name}`);
    }
  }
  assert.deepEqual(unresolvable, []);
});

test('full studio preview uses a reduced scale while smaller tiers stay full size', () => {
  assert.match(css, /\.overlay-preview-full\s*\{[^}]*--ovpv-scale:\s*0\.75;[^}]*width:\s*240px;[^}]*height:\s*72px;/s);
  assert.match(css, /\.overlay-preview-full\.has-pill\s*\{[^}]*height:\s*107px;/s);
  assert.match(preview, /\.ovpv-frame\s*\{[^}]*transform:\s*scale\(var\(--ovpv-scale, 1\)\);/s);
  assert.doesNotMatch(css, /\.overlay-preview-compact\s*\{[^}]*--ovpv-scale:/s);
  assert.doesNotMatch(css, /\.overlay-preview-bubble\s*\{[^}]*--ovpv-scale:/s);
});

test('selected agent rows keep the card edge unclipped', () => {
  assert.match(css, /\.settings-card-selected\s*\{[^}]*border-color:/s);
  assert.doesNotMatch(css, /\.selection-row\.selected\s*\{[^}]*box-shadow:/s);
});
