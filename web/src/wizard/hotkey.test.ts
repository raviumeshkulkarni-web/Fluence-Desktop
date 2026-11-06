import test from 'node:test';
import assert from 'node:assert/strict';

import { buildWizardHotkey, displayHotkey, isAllowedWizardHotkey } from './hotkey.ts';

test('plus maps to equals and stays allowed', () => {
  assert.equal(
    buildWizardHotkey({
      ctrlKey: true,
      altKey: false,
      shiftKey: true,
      metaKey: false,
      key: '+',
      code: 'Equal',
    }),
    'Ctrl+Shift+=',
  );
  assert.equal(isAllowedWizardHotkey('Ctrl+Shift+='), true);
});

test('numpad plus keeps its own backend token', () => {
  assert.equal(
    buildWizardHotkey({
      ctrlKey: true,
      altKey: false,
      shiftKey: false,
      metaKey: false,
      key: '+',
      code: 'NumpadAdd',
    }),
    'Ctrl+NumpadAdd',
  );
  assert.equal(isAllowedWizardHotkey('Ctrl+NumpadAdd'), true);
});

test('bare keys are rejected unless they are function keys', () => {
  assert.equal(isAllowedWizardHotkey('A'), false);
  assert.equal(isAllowedWizardHotkey('F12'), true);
  assert.equal(isAllowedWizardHotkey('Ctrl+Shift'), false);
});

test('backend tokens display as familiar labels', () => {
  assert.equal(displayHotkey('Ctrl+Meta+Space'), 'Ctrl+Win+Space');
  assert.equal(displayHotkey('Ctrl+NumpadAdd'), 'Ctrl+NumPlus');
  assert.equal(displayHotkey('Ctrl+Shift+Space'), 'Ctrl+Shift+Space');
});
