import test from 'node:test';
import assert from 'node:assert/strict';

import {
  collectHistoryForExport,
  historyCsv,
  historyTxt,
  localStamp,
  saveTextFile,
} from './historyExport.ts';
import type { HistoryEntry } from '@/ipc/history';

function entry(overrides: Partial<HistoryEntry> = {}): HistoryEntry {
  return {
    id: 'id-1',
    timestamp: '2026-03-09 14:05:00',
    text: 'hello world',
    mode: 'dictate',
    duration_ms: 1234,
    provider: 'openai',
    char_count: 11,
    ...overrides,
  };
}

test('historyCsv quotes every cell and doubles embedded quotes', () => {
  const rows = historyCsv([
    entry({ text: 'say "hi", then', model: null, language: null }),
  ]);
  assert.equal(
    rows,
    '"timestamp","text","provider","mode","duration_ms","language","model"\r\n' +
      '"2026-03-09 14:05:00","say ""hi"", then","openai","dictate","1234","",""\r\n',
  );
});

test('historyCsv neutralizes spreadsheet formula injection', () => {
  const dangerous = [
    '=cmd|calc!A0',
    '+1+cmd',
    '-2+3',
    '@SUM(1)',
    '\tcmd',
    '\rcmd',
    '   =cmd|calc!A0',
    '  +1+cmd',
    ' -2+3',
    '   @SUM(1)',
  ];
  for (const text of dangerous) {
    const rows = historyCsv([entry({ text })]);
    assert.ok(rows.includes(`"'${text}"`), text);
  }
});

test('historyTxt separates entries with a blank line and ends with a newline', () => {
  const out = historyTxt([
    entry({ timestamp: 't1', text: 'one' }),
    entry({ timestamp: 't2', text: 'two' }),
  ]);
  assert.equal(out, 't1\none\n\nt2\ntwo\n');
});

test('localStamp formats local wall time as YYYYMMDD-HHMMSS', () => {
  // Month/day/hours padded; constructed from getFullYear/getMonth/etc, so
  // it follows local time regardless of timezone.
  const stamp = localStamp(new Date(2026, 2, 9, 7, 5, 3));
  assert.equal(stamp, '20260309-070503');
});

test('collectHistoryForExport pages to the end and de-duplicates shifted rows', async () => {
  const pages = [
    [entry({ id: 'a' }), entry({ id: 'b' })],
    [entry({ id: 'b' }), entry({ id: 'c' })],
    [],
  ];
  const requested: number[] = [];
  const all = await collectHistoryForExport(async (page) => {
    requested.push(page);
    return pages[page] ?? [];
  });

  assert.deepEqual(requested, [0, 1, 2]);
  assert.deepEqual(
    all.map((row) => row.id),
    ['a', 'b', 'c'],
  );
});

// window stubs for the injected Tauri IPC; deleted in finally so tests run
// independently regardless of execution order.
type Globals = Record<string, unknown> & { window?: unknown };

function setWindow(invoke: (cmd: string, args?: Record<string, unknown>) => Promise<unknown>) {
  (globalThis as unknown as Globals).window = {
    __TAURI__: { core: { invoke } },
  };
}

function clearWindow() {
  delete (globalThis as unknown as Globals).window;
}

test('saveTextFile delegates the native save dialog to the gated backend', async () => {
  const calls: Array<{ cmd: string; args?: Record<string, unknown> }> = [];
  setWindow(async (cmd, args) => {
    calls.push({ cmd, args });
    return true;
  });
  try {
    const result = await saveTextFile('body', 'fluence-history-x.txt', 'text/plain');
    assert.equal(result, 'saved');
    assert.deepEqual(calls, [
      {
        cmd: 'save_export_file',
        args: { content: 'body', filename: 'fluence-history-x.txt' },
      },
    ]);
  } finally {
    clearWindow();
  }
});

test('saveTextFile reports cancelled when the backend dialog is dismissed', async () => {
  setWindow(async () => false);
  try {
    const result = await saveTextFile('body', 'fluence-history-x.csv', 'text/csv');
    assert.equal(result, 'cancelled');
  } finally {
    clearWindow();
  }
});

test('saveTextFile throws when the backend write fails so callers can toast', async () => {
  setWindow(async () => {
    throw new Error('disk full');
  });
  try {
    await assert.rejects(
      () => saveTextFile('{}', 'fluence-history-x.json', 'application/json'),
      /disk full/,
    );
  } finally {
    clearWindow();
  }
});

test('saveTextFile falls back to a browser download without Tauri globals', async () => {
  const clicked: string[] = [];
  const revoked: string[] = [];
  const globals = globalThis as unknown as Globals;
  const originalDocument = globals.document;
  const originalCreate = URL.createObjectURL;
  const originalRevoke = URL.revokeObjectURL;
  globals.document = {
    createElement: () => ({
      href: '',
      download: '',
      click() {
        clicked.push(this.download);
      },
    }),
  };
  globals.window = {};
  URL.createObjectURL = () => 'blob:test';
  URL.revokeObjectURL = (url: string) => revoked.push(url);
  try {
    const result = await saveTextFile('body', 'fluence-history-x.txt', 'text/plain');
    assert.equal(result, 'saved');
    assert.deepEqual(clicked, ['fluence-history-x.txt']);
    // Revocation is deferred (N2): the anchor click must not be undone in
    // the same tick.
    assert.equal(revoked.length, 0);
    await new Promise((resolve) => setTimeout(resolve, 1100));
    assert.deepEqual(revoked, ['blob:test']);
  } finally {
    globals.document = originalDocument;
    delete globals.window;
    URL.createObjectURL = originalCreate;
    URL.revokeObjectURL = originalRevoke;
  }
});
