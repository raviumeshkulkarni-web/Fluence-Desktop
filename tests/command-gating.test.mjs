// IPC gating invariants.
//
// Two layers:
//
// 1. COVERAGE (exact-equality). Every `#[tauri::command]` in src-tauri/src is
//    enumerated by parsing, not by a hand-written list, and each one's body is
//    brace-matched to decide whether it calls `require_caller`. The set of
//    commands that are NOT gated must equal EXPECTED_OPEN exactly. This fails
//    in both directions on purpose:
//      - a new ungated command appears  -> coverage regressed, gate it
//      - a command gets gated            -> shrink EXPECTED_OPEN in the same PR
//    That is what makes the remaining debt visible and non-expanding instead
//    of silently invisible.
//
// 2. LABELS (per-command). The security-critical commands are pinned to their
//    exact allow-list, so a gate cannot be widened by editing the table above.
//
// This replaces the earlier 22-row hand-maintained table, which could only
// ever check the commands it already knew about and could not detect a new
// ungated command at all.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync, readdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join, basename } from 'node:path';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const rustRoot = join(root, 'src-tauri', 'src');
const src = (p) => readFileSync(join(rustRoot, p), 'utf8');

const O = 'crate::acl::OVERLAY_WINDOW';
const M = 'crate::acl::MAIN_WINDOW';
const W = 'crate::acl::WIZARD_WINDOW';

// Commands intentionally left ungated, as `file.rs:fn_name`.
//
// AUD-03 remainder. Each needs a real per-caller audit before it can be gated:
// `start_recording` is reached from the wizard's mic test, `save_history_entry`
// is invoked fire-and-forget from the overlay (`invoke(...).catch(...)` with no
// user-visible error), so a wrong allow-list fails silently rather than loudly.
// Gate these in a follow-up PR that can exercise each call site.
//
// Deliberately open (low risk, documented in src-tauri/src/acl.rs):
//   get_app_version, get_hotkey_state, list_audio_devices,
//   get_dictionary, get_settings
// Commands whose OWN body carries no `require_caller`, as detected by the
// brace-matching scan below. This is the BODY-LEVEL open set, not the
// effective one: `import_dictionary` and `import_dictionary_report` delegate
// to `import_dictionary_impl`, which does gate. They are listed because the
// scan cannot see through a call, and a separate test below asserts that the
// delegate keeps its gate - so the effective open surface is 11, not 13.
const EXPECTED_OPEN = [
  'audio.rs:is_recording',
  'audio.rs:list_audio_devices',
  'audio.rs:start_recording',
  'audio.rs:stop_recording',
  'dictionary.rs:export_dictionary',
  'dictionary.rs:get_dictionary',
  'dictionary.rs:import_dictionary',
  'dictionary.rs:import_dictionary_report',
  'history.rs:get_history',
  'history.rs:save_history_entry',
  'hotkey.rs:get_hotkey_state',
  'main.rs:get_app_version',
  'settings.rs:get_settings',
];

// Commands listed above only because they gate via a shared implementation.
const DELEGATED_GATES = [
  ['dictionary.rs', 'import_dictionary', 'import_dictionary_impl'],
  ['dictionary.rs', 'import_dictionary_report', 'import_dictionary_impl'],
];

function rustFiles(dir) {
  const out = [];
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const p = join(dir, entry.name);
    if (entry.isDirectory()) out.push(...rustFiles(p));
    else if (entry.name.endsWith('.rs')) out.push(p);
  }
  return out;
}

/** Return every ungated `#[tauri::command]` as `file.rs:fn_name`. */
function ungatedCommands() {
  const open = [];
  for (const abs of rustFiles(rustRoot)) {
    const text = readFileSync(abs, 'utf8');
    const decl = /#\[tauri::command\][\s\S]{0,300}?fn\s+(\w+)\s*\(/g;
    let m;
    while ((m = decl.exec(text)) !== null) {
      const name = m[1];
      // Brace-match from the end of the signature to the fn body's closing
      // brace. Format-independent, so `cargo fmt` cannot break this test.
      const start = text.indexOf('{', m.index + m[0].length);
      if (start === -1) continue;
      let depth = 0;
      let end = -1;
      for (let i = start; i < text.length; i += 1) {
        if (text[i] === '{') depth += 1;
        else if (text[i] === '}') {
          depth -= 1;
          if (depth === 0) {
            end = i;
            break;
          }
        }
      }
      const body = end === -1 ? '' : text.slice(start, end);
      if (!/require_caller/.test(body)) open.push(`${basename(abs)}:${name}`);
    }
  }
  return open.sort();
}

test('every tauri command is gated, or is a pinned known-open command', () => {
  const actual = ungatedCommands();
  assert.deepEqual(
    actual,
    EXPECTED_OPEN,
    'ungated tauri commands drifted from the pinned set.\n' +
      `  newly ungated: ${actual.filter((x) => !EXPECTED_OPEN.includes(x)).join(', ') || '(none)'}\n` +
      `  now gated (drop from EXPECTED_OPEN): ${EXPECTED_OPEN.filter((x) => !actual.includes(x)).join(', ') || '(none)'}\n` +
      '  see EXPECTED_OPEN in tests/command-gating.test.mjs',
  );
});

test('commands gated via a shared implementation keep that gate', () => {
  // Guards the delegation gap the body scan cannot see: if someone drops the
  // gate from the shared impl, these two commands silently become ungated.
  for (const [file, caller, impl] of DELEGATED_GATES) {
    const text = src(file);
    const implAt = text.indexOf(`fn ${impl}(`);
    assert.notEqual(implAt, -1, `${file}: ${impl} not found`);
    const implSrc = text.slice(implAt, text.indexOf('\n}', implAt));
    assert.match(
      implSrc,
      /require_caller\(/,
      `${file}: ${impl} lost its gate, so ${caller} is now ungated`,
    );
    const callerAt = text.indexOf(`fn ${caller}(`);
    assert.notEqual(callerAt, -1, `${file}: ${caller} not found`);
    const callerSrc = text.slice(callerAt, text.indexOf('\n}', callerAt));
    assert.ok(
      callerSrc.includes(impl),
      `${file}: ${caller} must delegate to ${impl} to inherit its gate`,
    );
  }
});

// Security-critical commands pinned to their exact allow-list. A gate widened
// without review fails here even though it still "has" a require_caller.
const labels = [
  ['clipboard.rs', 'inject_text', [O]],
  ['clipboard.rs', 'copy_text', [O, M]],
  ['clipboard.rs', 'execute_keyboard_action', [O]],
  ['clipboard.rs', 'grab_active_selection', [O]],
  ['workflow.rs', 'stop_and_transcribe_recording', [O]],
  ['workflow.rs', 'finish_transcription_flow', [O]],
  ['workflow.rs', 'retry_transcription_flow', [O]],
  ['app_icon.rs', 'get_foreground_app_icon', [O]],
  ['overlay.rs', 'show_overlay', [O]],
  ['overlay.rs', 'hide_overlay', [O]],
  ['overlay.rs', 'set_overlay_style', [O]],
  ['overlay.rs', 'show_main_window', [M, W]],
  ['overlay.rs', 'hide_main_window', [M]],
  ['overlay.rs', 'minimize_main_window', [M]],
  ['overlay.rs', 'toggle_maximize_main_window', [M]],
  ['overlay.rs', 'show_wizard_window', [M, W]],
  ['overlay.rs', 'minimize_wizard', [W]],
  ['overlay.rs', 'close_wizard', [W]],
  ['offline_downloader.rs', 'get_offline_model_status', [M]],
  ['offline_downloader.rs', 'get_moonshine_v2_small_model_status', [M]],
  ['offline_downloader.rs', 'get_moonshine_v2_medium_model_status', [M]],
];

function gateFor(file, fn) {
  const text = src(file);
  const start = text.indexOf(`fn ${fn}(`);
  assert.notEqual(start, -1, `${file}: ${fn} not found`);
  // Brace-match the body rather than using a fixed window length.
  let brace = text.indexOf('{', start);
  assert.notEqual(brace, -1, `${file}: ${fn} has no body`);
  let depth = 0;
  let end = -1;
  for (let i = brace; i < text.length; i += 1) {
    if (text[i] === '{') depth += 1;
    else if (text[i] === '}') {
      depth -= 1;
      if (depth === 0) {
        end = i;
        break;
      }
    }
  }
  const body = text.slice(brace, end === -1 ? text.length : end);
  const at = body.indexOf('require_caller');
  assert.notEqual(at, -1, `${file}: ${fn} has no require_caller gate`);
  // Paren-match the call rather than searching for a trailing `;`: some gates
  // are expression-position (`require_caller(..).is_ok() && ...`) and have no
  // semicolon at all.
  const open = body.indexOf('(', at);
  let pdepth = 0;
  let close = -1;
  for (let i = open; i < body.length; i += 1) {
    if (body[i] === '(') pdepth += 1;
    else if (body[i] === ')') {
      pdepth -= 1;
      if (pdepth === 0) {
        close = i;
        break;
      }
    }
  }
  assert.notEqual(close, -1, `${file}: ${fn} require_caller call is unbalanced`);
  return body.slice(at, close + 1);
}

test('each gated command calls require_caller with exactly the intended labels', () => {
  for (const [file, fn, allowed] of labels) {
    const gate = gateFor(file, fn);
    for (const label of allowed) {
      assert.ok(
        gate.includes(label),
        `${file}: ${fn} must allow ${label}; gate was: ${gate}`,
      );
    }
    for (const label of [O, M, W].filter((x) => !allowed.includes(x))) {
      assert.ok(
        !gate.includes(label),
        `${file}: ${fn} must NOT allow ${label}; gate was: ${gate}`,
      );
    }
  }
});

test('no accidental empty allow-lists', () => {
  const gatedFiles = new Set(labels.map(([f]) => f));
  for (const file of gatedFiles) {
    const text = src(file);
    assert.ok(
      /\brequire_caller\(\s*&window/.test(text),
      `${file} should contain gated commands`,
    );
    assert.ok(!/&\[\s*\]/.test(text), `${file}: empty allow-list found`);
  }
});

test('acl.rs no longer advertises an unfixed open list', () => {
  const acl = src('acl.rs');
  assert.ok(!acl.includes('NOT FIXED'), 'stale NOT-FIXED section should be refreshed');
  assert.ok(acl.includes('Deliberately open'), 'open commands must be documented');
  assert.ok(!acl.includes('1.25.1'), 'caller table version stamp must be current');
});