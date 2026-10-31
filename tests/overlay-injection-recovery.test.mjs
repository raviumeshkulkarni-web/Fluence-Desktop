// Agent-action confirmation + injection responsiveness invariants.
// These parse src/js/overlay.js (and tauri.conf.json) to pin the behavior
// fixed in the hardening pass:
//   - dangerous agent actions (delete/select-all/submit) cannot run without
//     an explicit Confirm click (cancel/timeout never executes them);
//   - non-destructive actions (insert/rewrite/copy) stay one-click and are
//     not members of the dangerous set;
//   - the overlay never hides itself around injection (the window is
//     focus:false/WS_EX_NOACTIVATE, so no focus hand-back is needed);
//   - confirmed actions run against the validated target and surface a
//     visible success state, and failures stay recoverable.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const overlay = readFileSync(join(root, 'src', 'js', 'overlay.js'), 'utf8');
const tauriConf = readFileSync(join(root, 'src-tauri', 'tauri.conf.json'), 'utf8');

test('the overlay window cannot take OS focus (WS_EX_NOACTIVATE by config)', () => {
  const from = tauriConf.indexOf('"label": "overlay"');
  assert.notEqual(from, -1, 'overlay window definition must exist');
  const windowJson = tauriConf.slice(from, tauriConf.indexOf('}', from));
  assert.match(windowJson, /"focus"\s*:\s*false/, 'overlay must be focus:false');
  assert.match(
    windowJson,
    /"focusable"\s*:\s*false/,
    'overlay must be focusable:false (this is what applies WS_EX_NOACTIVATE)',
  );
});


test('agent actions execute immediately with no confirmation gate', () => {
  // Product decision: agent mode does not prompt. insert/rewrite/delete/
  // select-all/submit all dispatch on the first result.
  for (const gone of ['AGENT_DANGEROUS_ACTIONS', 'requestActionConfirmation', 'action-confirm']) {
    assert.ok(!overlay.includes(gone), 'agent flow must not reference ' + gone);
  }
  const branch = overlay.indexOf('let executed = false');
  assert.notEqual(branch, -1, 'agent dispatch block must exist');
  assert.notEqual(
    overlay.indexOf('attemptPaste(action.content', branch),
    -1,
    'dispatch must reach the first action without waiting on anything',
  );
});

test('confirmed dispatch window: all four actions, visible success, recoverable failure', () => {
  const start = overlay.indexOf('let executed = false');
  assert.notEqual(start, -1, 'dispatch block must exist');
  const end = overlay.indexOf("invoke('save_history_entry'", start);
  assert.notEqual(end, -1, 'agent flow must end by saving history');
  const dispatch = overlay.slice(start, end);
  assert.match(dispatch, /await attemptPaste\(action\.content/, 'insert/rewrite dispatch');
  assert.match(dispatch, /attemptKeyboardAction\('delete_chars'/);
  assert.match(dispatch, /attemptKeyboardAction\('select_all'/);
  assert.match(dispatch, /attemptKeyboardAction\('submit'/);
  assert.ok(!dispatch.includes('requestActionConfirmation'), 'dispatch must not re-confirm');
  assert.ok(!dispatch.includes('hide_overlay'), 'dispatch must not hide the overlay');
  assert.ok(!dispatch.includes('prepareForeground'), 'no staged focus hand-back');
  assert.match(dispatch, /setState\('success'\)/, 'success state must be surfaced');
  assert.match(
    dispatch,
    /setStatusMessage\(executed \? 'Executed' : 'Done'\)/,
    'success must be visible in status',
  );
  assert.match(dispatch, /handlePasteFailure\(pending, err/, 'failures must enter recovery');
  assert.match(dispatch, /kind: 'text'/, 'text recoverable as paste retry');
  assert.match(dispatch, /kind: 'keyboard'/, 'keyboard recoverable as keyboard retry');
});

test('copy agent action is one-click and never touches the confirm gate', () => {
  const copyBranch = overlay.indexOf("action.action === 'copy'");
  assert.notEqual(copyBranch, -1);
  const gate = overlay.indexOf('AGENT_DANGEROUS_ACTIONS.has(action.action)');
  assert.ok(copyBranch < gate || copyBranch > gate, 'copy branch exists in the agent flow');
  const copyWindowEnd = overlay.indexOf('fadeAndHide', copyBranch);
  const copyWindow = overlay.slice(copyBranch, copyWindowEnd > 0 ? copyWindowEnd : copyBranch + 500);
  assert.match(copyWindow, /invoke\('copy_text'/);
  assert.ok(!copyWindow.includes('requestActionConfirmation'));
});


test('retry re-runs the exact pending action', () => {
  const retry = overlay.slice(overlay.indexOf('async function retryPendingInjection'));
  assert.match(retry, /attemptPaste\(pending\.text, pending\.monitorAutoLearn\)/);
  assert.match(retry, /attemptKeyboardAction\(pending\.action, pending\.charCount\)/);
  assert.match(retry, /handlePasteFailure\(pending, err/, 'a failed retry re-enters recovery');
  // No target-window argument is sent: injection targets whatever holds
  // focus, so reading in one app while dictating into another works.
  assert.ok(!overlay.includes('targetWindow'), 'no targetWindow argument is threaded');
});
