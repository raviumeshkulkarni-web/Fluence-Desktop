import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

const ROOT = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const historyPath = path.join(ROOT, 'web/src/routes/HistoryPage.tsx');
const cssPath = path.join(ROOT, 'web/src/app.css');

test('history rows expose checkbox selection with the shadcn selected data state', () => {
  const history = readFileSync(historyPath, 'utf8');
  assert.match(history, /import \{ Checkbox \} from '@\/components\/ui\/checkbox'/);
  assert.match(history, /data-state=\{selected \? 'selected' : undefined\}/);
  assert.match(history, /<Checkbox[\s\S]*?checked=\{selected\}[\s\S]*?onCheckedChange=/);
  assert.match(history, /className=\{cn\(\s*'history-item'/);
});

test('history footer shows a static rows-per-page label instead of selection totals', () => {
  const history = readFileSync(historyPath, 'utf8');
  assert.match(history, /Rows per page: \{HISTORY_PAGE_SIZE\}/);
  assert.doesNotMatch(history, /row\(s\) selected/);
  assert.match(history, /history-selection-count[\s\S]*selectedIds\.size/);
});

test('history selection paint uses semantic shadcn tokens without the custom accent rail', () => {
  const css = readFileSync(cssPath, 'utf8');
  assert.match(css, /\.history-item\[data-state='selected'\][^{]*\{[^}]*background:\s*var\(--color-selected\);/s);
  const selectedRuleStart = css.indexOf(".history-item[data-state='selected']");
  const selectedRuleEnd = css.indexOf('}', selectedRuleStart);
  assert.notEqual(selectedRuleStart, -1);
  assert.doesNotMatch(css.slice(selectedRuleStart, selectedRuleEnd), /box-shadow|inset|border-left/);
});
