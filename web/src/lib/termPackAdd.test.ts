import test from 'node:test';
import assert from 'node:assert/strict';

import { addTermPacks } from './termPackAdd.ts';
import type { TermPack } from './termPacks.ts';

function pack(id: string, entries: [string, string][]): TermPack {
  return {
    id,
    name: id,
    description: id,
    entries: entries.map(([spoken, corrected]) => ({ spoken, corrected })),
  };
}

test('addTermPacks skips existing, adds new, and continues after a failure', async () => {
  const attempted: string[] = [];
  const result = await addTermPacks(
    [
      pack('one', [
        ['Alpha', 'A'],
        ['beta', 'B'],
        ['gamma', 'C'],
      ]),
      pack('two', [['delta', 'D']]),
    ],
    [' alpha '],
    async (spoken) => {
      attempted.push(spoken);
      if (spoken === 'gamma') throw new Error('disk full');
    },
  );

  assert.deepEqual(attempted, ['beta', 'gamma', 'delta']);
  assert.equal(result.added, 2);
  assert.equal(result.skipped, 1);
  assert.equal(result.failed, 1);
  assert.match(String(result.firstError), /disk full/);
});

test('addTermPacks treats backend duplicate errors as skips, not failures', async () => {
  const result = await addTermPacks(
    [pack('one', [['alpha', 'A'], ['beta', 'B']])],
    [],
    async (spoken) => {
      if (spoken === 'beta') throw new Error("Dictionary entry 'beta' already exists");
    },
  );

  assert.equal(result.added, 1);
  assert.equal(result.skipped, 1);
  assert.equal(result.failed, 0);
  assert.equal(result.firstError, null);
});
