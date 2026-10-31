import { test } from 'node:test';
import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import { extractFunction } from './extract-js-function.mjs';

const ROOT = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const source = extractFunction(path.join(ROOT, 'src/js/overlay.js'), 'setupHotkeyBusyFeedback');

function createBusyHarness(initialState) {
  const calls = [];
  let listener;
  const listen = async (eventName, callback) => {
    assert.equal(eventName, 'hotkey-busy');
    listener = callback;
  };
  const setStatusMessage = (message) => calls.push(['status', message]);
  const scheduleStatusReset = (message, delay) => calls.push(['reset', message, delay]);
  const scheduleAutoDismiss = (delay) => calls.push(['dismiss', delay]);
  const factory = new Function(
    'listen',
    'setStatusMessage',
    'scheduleStatusReset',
    'scheduleAutoDismiss',
    `let currentState = ${JSON.stringify(initialState)};
${source.replace('function setupHotkeyBusyFeedback', 'async function setupHotkeyBusyFeedback')}
return { setup: setupHotkeyBusyFeedback, setState: (state) => { currentState = state; } };`,
  );
  const handlers = factory(listen, setStatusMessage, scheduleStatusReset, scheduleAutoDismiss);
  return { calls, handlers, emit: async (payload) => listener({ payload }) };
}

test('hotkey busy feedback never auto-dismisses an active recording', async () => {
  for (const state of ['recording', 'agent', 'transcribing', 'agent_transcribing']) {
    const harness = createBusyHarness(state);
    await harness.handlers.setup();
    await harness.emit({ requested: 'hotkey-start-recording', active_owner: 2 });

    assert.deepEqual(harness.calls, [
      ['status', 'Recording busy'],
      ['reset', 'Recording busy', 1200],
    ]);
  }
});
