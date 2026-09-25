import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import { extractFunction, evalFunctions } from './extract-js-function.mjs';

const ROOT = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const overlayJsPath = path.join(ROOT, 'src/js/overlay.js');
const overlayCssPath = path.join(ROOT, 'src/css/overlay.css');
const overlayHtmlPath = path.join(ROOT, 'src/overlay.html');
const overlayRsPath = path.join(ROOT, 'src-tauri/src/overlay.rs');
const mainRsPath = path.join(ROOT, 'src-tauri/src/main.rs');

const js = () => readFileSync(overlayJsPath, 'utf8');
const css = () => readFileSync(overlayCssPath, 'utf8');
const rs = () => readFileSync(overlayRsPath, 'utf8');

// ── The expandable card is fully out of the release ────────────────────

test('no picker or card markup remains in the overlay document', () => {
  const html = readFileSync(overlayHtmlPath, 'utf8');
  for (const id of ['agent-picker', 'agent-strip', 'agent-card', 'agent-list', 'agent-strip-name', 'agent-strip-chevron']) {
    assert.ok(!html.includes(id), `#${id} still present in overlay.html`);
  }
  assert.ok(!html.includes('agent-picker'), 'agent-picker still present in overlay.html');
  // The bubble and the app pill are the whole overlay surface now.
  assert.match(html, /id="overlay-root"/);
  assert.match(html, /id="app-pill"/);
});

test('the card window-resize IPC is removed from the frontend and backend', () => {
  assert.ok(!js().includes('set_agent_picker_height'), 'overlay.js still invokes set_agent_picker_height');
  const rust = rs();
  assert.ok(!rust.includes('set_agent_picker_height'), 'overlay.rs still defines set_agent_picker_height');
  assert.ok(!rust.includes('AGENT_PICKER_WINDOW_WIDTH'), 'overlay.rs still defines AGENT_PICKER_WINDOW_WIDTH');
  assert.ok(
    !readFileSync(mainRsPath, 'utf8').includes('set_agent_picker_height'),
    'main.rs still registers set_agent_picker_height',
  );
});

test('no card-only CSS, variables or keyframes remain', () => {
  const sheet = css();
  for (const token of [
    '--agent-picker-card-height',
    '--agent-picker-extra',
    '--agent-picker-gap',
    '--agent-ref-top',
    '--agent-base-width',
    '--agent-bubble-shift-x',
  ]) {
    assert.ok(!sheet.includes(token), `${token} still declared in overlay.css`);
  }
  for (const selector of ['.agent-picker', '.agent-strip', '.agent-card', '.agent-list', '.agent-row-btn', '.agent-card-title']) {
    assert.ok(!sheet.includes(selector), `${selector} still present in overlay.css`);
  }
  assert.ok(!/agent-strip-in|agent-card-in|agent-strip-out|agent-card-out/.test(sheet), 'picker keyframes remain');
  assert.ok(!sheet.includes('agent-measuring'), '.agent-card.agent-measuring remains');
});

test('no card-only JavaScript remains in the overlay frontend', () => {
  const source = js();
  for (const fn of [
    'setAgentPickerExpanded',
    'measureAgentCardHeightStable',
    'computeAgentPickerExpansion',
    'renderAgentPicker',
    'toggleAgentCard',
    'setAgentPickerLayout',
    'setAgentBubbleShiftX',
    'readAgentTierCssPx',
    'computeAgentPickerGeometry',
    'applyAgentPickerGeometry',
    'applyAgentPickerPlacement',
    'clearAgentPickerTimers',
    'queueAgentPickerHeight',
    'waitForRenderFrame',
    'showAgentPicker',
    'hideAgentPicker',
    'loadAgentPicker',
    'preloadAgents',
    'agentDisplayName',
  ]) {
    assert.ok(!source.includes(fn), `${fn} still present in overlay.js`);
  }
  for (const state of ['agentPickerExpanded', 'agentPickerVisible', 'agentPickerPhase', 'agentPickerGen', 'AGENT_STRIP_EXTRA']) {
    assert.ok(!source.includes(state), `${state} still present in overlay.js`);
  }
});

// ── Preserved: Agent Mode still resolves and runs the chosen agent ────

test('agent mode seeds the active agent from the persisted default', async () => {
  const resolve = extractFunction(overlayJsPath, 'resolveDefaultAgentId');
  assert.match(resolve, /invoke\('get_agents'\)/);
  assert.match(resolve, /if \(!activeAgentId\) activeAgentId = defaultId;/);
  // Fail-closed: a store read error leaves activeAgentId null, which the
  // backend resolves to the built-in agent.
  assert.match(resolve, /catch \(err\)/);
  // Seeded on load, on pageshow, and at the start of every agent recording.
  const source = js();
  assert.match(source, /void resolveDefaultAgentId\(\);/);
  const start = extractFunction(overlayJsPath, 'setupEventListeners');
  assert.match(start, /await resolveDefaultAgentId\(\);/);
});

test('the agent start path passes the resolved agent id to execution', () => {
  const source = js();
  assert.match(source, /const turnAgentId = retryAgentId !== undefined \? retryAgentId : activeAgentId;/);
  assert.match(source, /agent_id:\s*turnAgentId/);
  assert.match(source, /execute_agent_command_secure/);
});

test('transcription mode never resolves or reads an agent id', () => {
  const start = extractFunction(overlayJsPath, 'setupEventListeners');
  // The STT branch drops the agent edge anchor so the bubble uses the
  // standard layout, exactly as before the rollback.
  assert.match(start, /setAgentPickerAnchor\('above', false\);/);
  const sttBranch = start.slice(start.indexOf("hotkey-start-recording'"), start.indexOf("hotkey-stop-recording'"));
  assert.ok(!sttBranch.includes('resolveDefaultAgentId'), 'STT branch must not resolve an agent id');
});

// ── Preserved: island / pill placement across every dock ──────────────

test('only the visible bubble captures pointer input', () => {
  const sheet = css();
  assert.match(sheet, /body\s*\{[^}]*pointer-events:\s*none;/s);
  assert.match(sheet, /\.overlay-root\.active\s*\{[^}]*pointer-events:\s*auto;/s);
  assert.match(sheet, /\.app-pill\s*\{[^}]*pointer-events:\s*none;/s);
});

test('the island is edge-anchored at a constant offset for both placements', () => {
  const sheet = css();
  // Bottom docks: bottom-anchored at a constant per-tier offset.
  assert.match(sheet, /body\.agent-placement-above\s*\{[^}]*align-items:\s*flex-end;[^}]*padding-top:\s*var\(--agent-stack-top\);[^}]*padding-bottom:\s*calc\(var\(--agent-base-height\) - var\(--agent-stack-top\) - var\(--agent-bubble-height\)\);/s);
  // Top docks: top-anchored 46px.
  assert.match(sheet, /body\.agent-placement-below\s*\{[^}]*align-items:\s*flex-start;[^}]*padding-top:\s*var\(--agent-stack-top\);/s);
  // Regression guard: the old fixed hacks that caused the +24px/+30px jumps.
  assert.doesNotMatch(sheet, /padding-bottom:\s*24px/);
  // Neither island offset may depend on the window height: the window is sized
  // to the tier and never resized during a session, so these are pure constants.
  const aboveBlock = sheet.match(/body\.agent-placement-above\s*\{[^}]*\}/);
  assert.ok(aboveBlock && !aboveBlock[0].includes('var(--agent-picker-extra'));
});

test('the app pill mirrors the island edge in both placements', () => {
  const sheet = css();
  assert.match(sheet, /\.app-pill\s*\{[^}]*top:\s*var\(--agent-pill-top, 8px\);/s);
  assert.match(sheet, /body\.agent-placement-below \.app-pill\s*\{[^}]*top:\s*var\(--agent-pill-top\);[^}]*bottom:\s*auto;/s);
  assert.match(sheet, /body\.agent-placement-above \.app-pill\s*\{[^}]*top:\s*auto;[^}]*bottom:\s*calc\(var\(--agent-base-height\) - var\(--agent-pill-top\) - 24px\);/s);
});

test('the bubble carries no horizontal picker-width compensation', () => {
  const sheet = css();
  // The window is never widened now, so no compensating translate is needed.
  assert.doesNotMatch(sheet, /translateX\(--agent-bubble-shift-x\)/);
  assert.doesNotMatch(sheet, /translateX\(calc\(-50% \+ var\(--agent-bubble-shift-x/);
  // Centring is plain.
  assert.match(sheet, /\.app-pill\.visible\s*\{[^}]*transform:\s*translateX\(-50%\) translateY\(0\);/s);
});

test('body padding and tier geometry are declared once in CSS, never measured', () => {
  const sheet = css();
  assert.match(sheet, /body\s*\{[^}]*padding-top:\s*var\(--agent-stack-top\);/s);
  assert.match(sheet, /body\.style-full\s*\{[^}]*--agent-bubble-height:\s*96px;[^}]*--agent-base-height:\s*190px;/s);
  assert.match(sheet, /body\.style-compact\s*\{[^}]*--agent-bubble-height:\s*36px;[^}]*--agent-base-height:\s*130px;/s);
  assert.match(sheet, /body\.style-bubble\s*\{[^}]*--agent-bubble-height:\s*44px;[^}]*--agent-base-height:\s*140px;/s);
  // Reading offsetHeight during the 36→96px entry transition used to pin an
  // intermediate bubble height for the whole session.
  const source = js();
  assert.ok(!source.includes('setAgentBubbleHeight'));
  assert.ok(!/setProperty\('--agent-bubble-height'/.test(source));
  assert.ok(!/addEventListener\('resize'/.test(source));
});

test('CSS tier geometry mirrors overlay.rs window sizes exactly', () => {
  const sheet = css();
  const rust = rs();
  const tuple = (key) => {
    const m = rust.match(new RegExp(`${key}"\\s*=>\\s*\\(([\\d.]+),\\s*([\\d.]+)\\)`));
    return m ? [Number(m[1]), Number(m[2])] : null;
  };
  const fallback = rust.match(/_\s*=>\s*\(([\d.]+),\s*([\d.]+)\)/);
  const sizes = {
    compact: tuple('compact'),
    bubble: tuple('bubble'),
    full: fallback ? [Number(fallback[1]), Number(fallback[2])] : null,
  };
  assert.ok(sizes.compact && sizes.bubble && sizes.full, 'overlay_window_size tuples not found');
  for (const [style, [, height]] of Object.entries(sizes)) {
    const block = sheet.match(new RegExp(`body\\.style-${style}\\s*\\{[^}]*\\}`));
    assert.ok(block, `body.style-${style} block missing`);
    assert.match(block[0], new RegExp(`--agent-base-height:\\s*${height}px;`));
  }
});

// Stable placement model: the island is pinned to the window edge the OS never
// moves. Bottom docks anchor above, top docks below; unknown future positions
// fall back to measured space with an above default.
const resolveAgentPickerPlacement = evalFunctions(
  [extractFunction(overlayJsPath, 'resolveAgentPickerPlacement')],
  'resolveAgentPickerPlacement',
);

test('the island opens away from the nearest edge for every docked position', () => {
  assert.equal(resolveAgentPickerPlacement('bottom_right', null, null), 'above');
  assert.equal(resolveAgentPickerPlacement('bottom_left', null, null), 'above');
  assert.equal(resolveAgentPickerPlacement('center', null, null), 'above');
  assert.equal(resolveAgentPickerPlacement('bottom_center', null, null), 'above');
  assert.equal(resolveAgentPickerPlacement('center_bottom', null, null), 'above');
  assert.equal(resolveAgentPickerPlacement('top_left', null, null), 'below');
  assert.equal(resolveAgentPickerPlacement('top_right', null, null), 'below');
  assert.equal(resolveAgentPickerPlacement('top_center', null, null), 'below');
  assert.equal(resolveAgentPickerPlacement('center_top', null, null), 'below');
});

test('unknown side positions fall back to measured space, clamped above by default', () => {
  assert.equal(
    resolveAgentPickerPlacement('center_left', { top: 100, bottom: 144, left: 10, right: 54 }, { width: 400, height: 600 }),
    'below',
  );
  assert.equal(
    resolveAgentPickerPlacement('center_right', { top: 450, bottom: 494, left: 340, right: 384 }, { width: 400, height: 600 }),
    'above',
  );
  assert.equal(resolveAgentPickerPlacement('mystery', null, null), 'above');
});

test('a docked hint is never re-flipped by window-local measurement', () => {
  // Placement resolves before `overlayRoot` is `active`, so the anchor rect is
  // window-local: the 36px box sits ~47px from the window top. Comparing that
  // against a 46px threshold read "more room below" for every bottom dock and
  // inverted them. The dock is authoritative.
  const localBubble = { top: 46.9, bottom: 81.1 };
  const tiers = [{ width: 400, height: 190 }, { width: 180, height: 130 }, { width: 130, height: 140 }];
  for (const vp of tiers) {
    for (const dock of ['bottom_right', 'bottom_left', 'center', 'bottom_center']) {
      assert.equal(resolveAgentPickerPlacement(dock, localBubble, vp), 'above', `${dock}@${vp.width}`);
    }
    for (const dock of ['top_right', 'top_left', 'top_center']) {
      assert.equal(resolveAgentPickerPlacement(dock, localBubble, vp), 'below', `${dock}@${vp.width}`);
    }
  }
  assert.equal(
    resolveAgentPickerPlacement('bottom_right', { top: 4, bottom: 40 }, { width: 400, height: 60 }),
    'above',
  );
});

test('the session edge anchor is set for the whole agent run', () => {
  const source = js();
  // The dock decides the anchor once, before the overlay is shown.
  assert.match(source, /setAgentPickerAnchor\(\s*resolveAgentPickerPlacement\(prefs\.overlayPosition/);
  // Cleared only after the window is hidden, so nothing shifts on screen.
  const fade = extractFunction(overlayJsPath, 'fadeAndHide');
  assert.ok(fade.indexOf("await invoke('hide_overlay')") < fade.indexOf('setAgentPickerAnchor(agentPickerPlacement, false)'));
  // The anchor function only toggles placement classes.
  const anchor = extractFunction(overlayJsPath, 'setAgentPickerAnchor');
  assert.match(anchor, /classList\.toggle\('agent-placement-above'/);
  assert.match(anchor, /classList\.toggle\('agent-placement-below'/);
});

// ── Preserved: overlay window lifecycle ───────────────────────────────

test('startup state does not activate the hidden overlay before the native show', () => {
  const setup = extractFunction(overlayJsPath, 'setupEventListeners');
  const setState = extractFunction(overlayJsPath, 'setState');
  assert.match(setup, /setState\('recording', false\)/);
  assert.match(setup, /setState\('agent', false\)/);
  assert.match(setState, /function setState\(state, updateVisibility = true\)/);
  assert.match(setState, /if \(overlayRoot && updateVisibility\)/);
});

test('session teardown hides the window before clearing the edge anchor', () => {
  const fade = extractFunction(overlayJsPath, 'fadeAndHide');
  assert.match(fade, /await new Promise\(r => setTimeout\(r, 200\)\)/);
  assert.match(fade, /overlayRoot\.classList\.remove\('active'\)/);
  const hideOverlayAt = fade.indexOf("await invoke('hide_overlay')");
  const anchorResetAt = fade.indexOf('setAgentPickerAnchor(agentPickerPlacement, false)');
  assert.ok(hideOverlayAt >= 0 && anchorResetAt > hideOverlayAt);
});

test('hiding the overlay always restores its base bounds', () => {
  const rust = rs();
  const hideStart = rust.indexOf('pub fn hide_overlay');
  const hideEnd = rust.indexOf('\n#[tauri::command]', hideStart);
  assert.ok(hideStart >= 0 && hideEnd > hideStart);
  assert.match(rust.slice(hideStart, hideEnd), /place_overlay_window\(/);
});

test('app pill fades out instead of popping', () => {
  const hide = extractFunction(overlayJsPath, 'hideAppPill');
  assert.match(hide, /pill\.classList\.remove\('visible'\);/);
  assert.ok(hide.indexOf('pill.hidden = true') === -1);
  assert.match(hide, /appPillHideTimer = setTimeout/);
  const show = extractFunction(overlayJsPath, 'applyAppInfo');
  assert.match(show, /if \(appPillHideTimer\) \{ clearTimeout\(appPillHideTimer\); appPillHideTimer = null; \}/);
});

test('status island maintains unified curvature across all docked corners', () => {
  const sheet = css();
  for (const corner of ['corner-bottom-left', 'corner-bottom-right', 'corner-bottom-center', 'corner-top-left', 'corner-top-right', 'corner-top-center']) {
    const block = sheet.match(new RegExp(`\\.overlay-root\\.${corner}\\s*\\{([\\s\\S]*?)\\}`));
    assert.ok(block, `Missing .overlay-root.${corner} block`);
    assert.match(block[1], /border-radius:\s*16px;/);
    assert.doesNotMatch(block[1], /\b6px\b/);
  }
});

test('native overlay geometry is committed atomically (single SetWindowPos)', () => {
  const rust = rs();

  // Bottom docks derive their window y from the requested height, so a resize
  // applied before the move leaves the window transiently too low. The Windows
  // commit path must therefore be a single SetWindowPos.
  const windowsCommit = rust.match(
    /#\[cfg\(windows\)\]\s*fn commit_overlay_geometry\([\s\S]*?\n\}/,
  );
  assert.ok(windowsCommit, 'cfg(windows) commit_overlay_geometry missing');
  const commit = windowsCommit[0];

  const setWindowPosCalls = commit.match(/SetWindowPos\(/g) || [];
  assert.equal(setWindowPosCalls.length, 1, 'Windows commit path must issue exactly one SetWindowPos');
  assert.doesNotMatch(commit, /\.set_size\(/, 'Windows commit path must not resize separately');
  assert.doesNotMatch(commit, /\.set_position\(/, 'Windows commit path must not move separately');
  assert.match(commit, /SetWindowPos\([\s\S]*?to_px\(x\)[\s\S]*?to_px\(y\)[\s\S]*?w_px[\s\S]*?h_px[\s\S]*?\)/);
  assert.match(commit, /SWP_NOZORDER \| SWP_NOACTIVATE/);

  const place = rust.match(/fn place_overlay_window_sized\([\s\S]*?\n\}/);
  assert.ok(place, 'place_overlay_window_sized missing');
  assert.match(place[0], /commit_overlay_geometry\(/);
  assert.doesNotMatch(place[0], /\.set_size\(/);
  assert.doesNotMatch(place[0], /\.set_position\(/);
});

test('overlay dock geometry keeps top and bottom symmetric', () => {
  const origin = rs().match(/fn overlay_window_origin\([\s\S]*?\n\}/);
  assert.ok(origin, 'overlay_window_origin missing');
  const body = origin[0];

  // All six placements: five explicit arms plus the `_` catch-all (bottom_right).
  for (const dock of ['top_left', 'top_center', 'top_right', 'bottom_left', 'center']) {
    assert.ok(body.includes(`"${dock}"`), `${dock} placement branch missing`);
  }
  assert.match(body, /"top_left" => \(margin, margin\)/);
  assert.match(body, /"top_center" => \(screen_width \/ 2\.0 - win_width \/ 2\.0, margin\)/);
  assert.match(body, /"top_right" => \(screen_width - win_width - margin, margin\)/);
  assert.equal(
    (body.match(/screen_height - win_height - margin - bottom_reserve/g) || []).length,
    3,
    'bottom_left, center and bottom_right must all derive y from window height',
  );
  assert.match(body, /_ => \{[\s\S]*?screen_height - win_height - margin - bottom_reserve/);
});
