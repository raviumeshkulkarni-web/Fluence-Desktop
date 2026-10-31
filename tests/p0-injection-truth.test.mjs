// P0 source invariants: truthful SendInput reporting + foreground target
// validation. These tests parse the Rust clipboard module and the overlay
// script as text, asserting the ordering and sentinels fixed in the P0
// package (paste-into-wrong-window and silent partial delivery).
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const clip = readFileSync(join(root, 'src-tauri', 'src', 'clipboard.rs'), 'utf8');
const overlay = readFileSync(join(root, 'src', 'js', 'overlay.js'), 'utf8');

function bodyOf(src, signature) {
  const start = src.indexOf(signature);
  assert.notEqual(start, -1, `expected ${signature}`);
  return src.slice(start);
}

test('inject_text targets the current focus window, with no target validation', () => {
  // Product decision: paste goes wherever the cursor is when transcription
  // finishes. Reading in one app while dictating and pasting into another is a
  // primary workflow, so no foreground capture/compare exists any more.
  const body = bodyOf(clip, 'pub async fn inject_text(');
  assert.ok(!body.includes('foreground_window_matches'), 'no foreground compare');
  assert.ok(!body.includes('TARGET_WINDOW_CHANGED'), 'no target-mismatch refusal');
  assert.ok(!body.includes('target_window'), 'no target window parameter');
  assert.ok(
    !clip.includes('capture_target_window'),
    'the capture command is gone entirely',
  );
  assert.ok(
    !clip.includes('foreground_window_matches'),
    'the match helper is gone entirely',
  );
});

test('inject_text reports partial paste delivery truthfully', () => {
  const body = bodyOf(clip, 'pub async fn inject_text(');
  assert.ok(body.includes('PASTE_FAILED:'), 'partial-delivery sentinel required');
  assert.ok(
    body.includes('ctrlv_sent == 4'),
    'paste_ok must be derived from the actual delivered key-event count',
  );
});

test('execute_keyboard_action reports partial delivery and rejects unknown actions', () => {
  const body = bodyOf(clip, 'pub async fn execute_keyboard_action(');
  assert.ok(body.includes('KEYBOARD_ACTION_FAILED:'));
  assert.ok(body.includes('Unknown keyboard action'));
  assert.ok(body.includes('delete_chars requires a charCount'));
  assert.ok(
    !body.includes('foreground_window_matches'),
    'no target validation gates the first keystroke',
  );
  assert.ok(body.includes('sleep(Duration::from_millis(80))'));
});

test('a failed injection stays recoverable, whatever the cause', () => {
  // Paste failure is reported specifically and remains retryable/copyable.
  assert.ok(overlay.includes('describeInjectError'), 'failures name their cause');
  assert.ok(overlay.includes('pendingInjection'), 'failed injections must be retained for retry');
  assert.ok(overlay.includes('retryPendingInjection'));
  assert.ok(overlay.includes('Copied - paste with Ctrl+V'));
  assert.ok(!overlay.includes('TARGET_WINDOW_CHANGED'), 'no target-mismatch refusal remains');
});

test('overlay keeps transcripts of superseded sessions unless the user discarded them', () => {
  const body = bodyOf(overlay, 'async function runSttFlow');
  assert.ok(
    body.includes('save_history_entry') && body.includes('discardedSessions'),
    'orphaned STT results must still be saved to history unless discarded',
  );
});

test('agent actions run without a confirmation prompt', () => {
  // Agent mode executes insert/rewrite/delete/select-all/submit directly.
  assert.ok(!overlay.includes('AGENT_DANGEROUS_ACTIONS'));
  assert.ok(!overlay.includes('requestActionConfirmation'));
  // Paste recovery (Retry / Copy) is independent of any prompt and remains.
  assert.ok(overlay.includes('handlePasteFailure'));
});

// The overlay window is created with "focus": false and "focusable": false
// (WS_EX_NOACTIVATE comes from focusable:false, tauri.conf.json): clicks on
// it never move the OS foreground window, so
// there is nothing to "restore" before injecting, and hiding the overlay
// first would only hide the recovery UI. Injections run against the
// still-foreground target, whose identity Inject already fail-closed-validates.
test('overlay never hides itself to hand back foreground before injecting', () => {
  assert.ok(
    !overlay.includes('prepareForegroundForInjection'),
    'focus-transfer helper must stay removed',
  );
  assert.ok(
    !overlay.includes('reshowOverlayForSession'),
    'reshow helper was only needed by the removed hide dance',
  );
  const execute = bodyOf(overlay, 'async function attemptKeyboardAction');
  const recovery = overlay.indexOf('function setupPasteRecovery');
  const windowAroundInjection = overlay.slice(
    execute,
    recovery > 0 ? recovery : overlay.length,
  );
  assert.ok(
    !windowAroundInjection.includes("invoke('hide_overlay')"),
    'injection/retry paths must not hide the overlay',
  );
});
