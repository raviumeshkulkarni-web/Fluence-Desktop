import test from 'node:test';
import assert from 'node:assert/strict';
import { parseReleaseNotes } from './release-notes.ts';

test('release markdown becomes readable sections and plain list items', () => {
  assert.deepEqual(
    parseReleaseNotes(
      '## Highlights\n\n### Dictation\n- **Faster** startup\n- Supports `NumPlus`\n\n### Fixes\nA clearer retry message.',
    ),
    [
      { title: 'Dictation', items: ['Faster startup', 'Supports NumPlus'] },
      { title: 'Fixes', items: ['A clearer retry message.'] },
    ],
  );
});

test('empty release bodies return no sections', () => {
  assert.deepEqual(parseReleaseNotes(null), []);
  assert.deepEqual(parseReleaseNotes('  \n'), []);
});
