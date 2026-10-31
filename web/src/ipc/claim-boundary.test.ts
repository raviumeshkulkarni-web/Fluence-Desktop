// Security-boundary tests for the claim IPC wrappers.
//
// The claim commands must carry ONLY a record id. The destination account is
// resolved by the backend from the durable session; if an account hash ever
// became a frontend parameter, any renderer compromise could aim a claim at an
// arbitrary account partition. This asserts that structurally by intercepting the
// invoke layer rather than by inspecting the call sites by eye.

import test from 'node:test';
import assert from 'node:assert/strict';

import { claimLegacyAgent } from './agents.ts';
import { claimLegacyPromptStyle } from './prompts.ts';

type Call = { cmd: string; args?: Record<string, unknown> };
const calls: Call[] = [];

// Stub the exact bridge `@/ipc/tauri` uses (`window.__TAURI__.core.invoke`).
//
// NOTE: this assignment does NOT run before the imports above — ES module
// imports are hoisted and evaluated first. It works because `invokeCmd` resolves
// `window.__TAURI__` LAZILY, on each call, rather than capturing it at module
// load. That is the behaviour under test: the stub must be sufficient at call
// time, not at import time.
(globalThis as Record<string, unknown>).window = {
  __TAURI__: {
    core: {
      invoke: (cmd: string, args?: Record<string, unknown>) => {
        calls.push({ cmd, args });
        return Promise.resolve({
          claimedIds: [],
          skippedIds: [],
          refusedIds: [],
        });
      },
    },
  },
};

test('claiming an agent sends only the id', async () => {
  calls.length = 0;
  await claimLegacyAgent('agent:abc');
  assert.equal(calls.length, 1);
  assert.equal(calls[0]!.cmd, 'claim_legacy_agent');
  assert.deepEqual(Object.keys(calls[0]!.args ?? {}), ['id']);
});

test('claiming a style sends only the id', async () => {
  calls.length = 0;
  await claimLegacyPromptStyle('custom:abc');
  assert.equal(calls[0]!.cmd, 'claim_legacy_prompt_style');
  assert.deepEqual(Object.keys(calls[0]!.args ?? {}), ['id']);
});

test('no claim call can carry an account, hash, or destination', async () => {
  const forbidden = /account|hash|owner|destination|partition|email/i;
  for (const id of ['agent:1', 'custom:2']) {
    calls.length = 0;
    await claimLegacyAgent(id);
    await claimLegacyPromptStyle(id);
    for (const call of calls) {
      for (const key of Object.keys(call.args ?? {})) {
        assert.ok(
          !forbidden.test(key),
          `claim command must not accept "${key}" — the backend owns account selection`,
        );
      }
    }
  }
});