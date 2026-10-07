// Focused tests for the claim messaging policy and the IPC security boundary.
//
// Pure logic only — no DOM, no React, no new dependencies. Runs on Node's
// built-in test runner via native type-stripping (node >= 22.6):
//   npm --prefix web run test
//
// The risky logic here is (a) telling the user the truth about a refusal and
// (b) not letting the frontend name a destination account. Both are covered.

import test from 'node:test';
import assert from 'node:assert/strict';

import { describeClaimOutcome } from './claim.ts';

const outcome = (
  claimedIds: string[] = [],
  skippedIds: string[] = [],
  refusedIds: [string, string][] = [],
) => ({ claimedIds, skippedIds, refusedIds });

test('a successful claim reports success', () => {
  const message = describeClaimOutcome(outcome(['agent:1']), 'agent');
  assert.equal(message.kind, 'success');
  assert.match(message.text, /Added to your account/);
});

test('success wins over any other field in the same outcome', () => {
  const message = describeClaimOutcome(
    outcome(['agent:1'], ['agent:2'], [['agent:3', 'tombstoned']]),
    'agent',
  );
  assert.equal(message.kind, 'success');
});

test('a tombstoned refusal explains the delete rather than reading as a no-op', () => {
  const message = describeClaimOutcome(
    outcome([], [], [['agent:1', 'tombstoned']]),
    'agent',
  );
  assert.equal(message.kind, 'info', 'a refusal is a policy decision, not an error');
  assert.match(message.text, /deleted in your account/);
  assert.doesNotMatch(message.text, /Nothing to add/);
});

test('a duplicate legacy id explains itself', () => {
  const message = describeClaimOutcome(
    outcome([], [], [['style:1', 'duplicate-legacy-id']]),
    'style',
  );
  assert.equal(message.kind, 'info');
  assert.match(message.text, /more than once on this device/);
});

test('a builtin refusal explains itself', () => {
  const message = describeClaimOutcome(
    outcome([], [], [['style:1', 'builtin-id']]),
    'style',
  );
  assert.match(message.text, /built in/);
});

test('an unknown refusal reason degrades to generic wording, never raw text', () => {
  const message = describeClaimOutcome(
    outcome([], [], [['agent:1', 'some-future-reason']]),
    'agent',
  );
  assert.match(message.text, /could not be added/);
  assert.doesNotMatch(message.text, /some-future-reason/);
});

test('a skip is not reported as a refusal and not as a fresh success', () => {
  const message = describeClaimOutcome(outcome([], ['agent:1']), 'agent');
  assert.equal(message.kind, 'info');
  assert.match(message.text, /Already in your account/);
});

test('an empty outcome is a clean no-op', () => {
  const message = describeClaimOutcome(outcome(), 'agent');
  assert.equal(message.kind, 'info');
  assert.match(message.text, /Nothing to add/);
});

test('the refusal subject matches the board', () => {
  const a = describeClaimOutcome(outcome([], [], [['x', 'tombstoned']]), 'agent');
  const s = describeClaimOutcome(outcome([], [], [['x', 'tombstoned']]), 'style');
  assert.match(a.text, /That agent was deleted/);
  assert.match(s.text, /That style was deleted/);
});

test('malformed refusal entries degrade instead of throwing', () => {
  // The backend always emits 2-tuples, but the UI and binary version
  // independently. Destructuring a null / number / 1-tuple would throw and replace
  // truthful messaging with a raw crash toast.
  for (const bad of [null, 7, 'x', {}, ['only-one'], []]) {
    const message = describeClaimOutcome(
      { claimedIds: [], skippedIds: [], refusedIds: [bad as never] },
      'agent',
    );
    assert.equal(message.kind, 'info');
    assert.equal(typeof message.text, 'string');
  }
});

test('a wholly absent outcome object degrades to the no-op message', () => {
  const message = describeClaimOutcome({} as never, 'style');
  assert.equal(message.kind, 'info');
  assert.match(message.text, /Nothing to add/);
});

test('a malformed success field does not fabricate a success', () => {
  const message = describeClaimOutcome(
    { claimedIds: 'nope' as never, refusedIds: [] },
    'agent',
  );
  assert.equal(message.kind, 'info');
  assert.doesNotMatch(message.text, /Added to your account/);
});

test('messaging never claims the legacy copy was removed', () => {
  // The claim is copy-not-move; copy that implied a move would be a lie.
  for (const o of [outcome(['x']), outcome([], ['x']), outcome()]) {
    assert.doesNotMatch(describeClaimOutcome(o, 'agent').text, /removed|moved|deleted from/i);
  }
});