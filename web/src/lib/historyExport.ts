// History export helpers. Pure string/blob builders so the export dialog
// stays thin and the formatting is unit-testable (historyExport.test.ts).
// Also hosts saveTextFile: the shared text-saving entry point for the
// history and dictionary exports.
import type { HistoryEntry } from '@/ipc/history';
import { invokeCmd } from '@/ipc/tauri';

export function downloadFile(content: string, filename: string, type: string) {
  const blob = new Blob([content], { type });
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = filename;
  a.click();
  // Deferred revoke: the download has started by then, and revoking inside
  // the same tick can cancel it in some WebView versions.
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}

function hasTauriCore(): boolean {
  const w = window as unknown as {
    __TAURI__?: { core?: { invoke?: unknown } };
  };
  return typeof w.__TAURI__?.core?.invoke === 'function';
}

export type SaveFileResult = 'saved' | 'cancelled';

/**
 * Persist text content. Inside Tauri the blob-anchor download is unreliable
 * in WebView2 (the file may never land in Downloads), so a gated backend
 * command opens the native save dialog and writes the user-picked path.
 * The main window therefore needs no fs plugin permission. Falls back to a
 * browser download when the Tauri IPC is absent (plain browser dev).
 * Throws on failure so callers can toast; 'cancelled' means the user closed
 * the save dialog without picking a location.
 */
export async function saveTextFile(
  content: string,
  filename: string,
  mime: string,
): Promise<SaveFileResult> {
  if (!hasTauriCore()) {
    downloadFile(content, filename, mime);
    return 'saved';
  }
  const saved = await invokeCmd<boolean>('save_export_file', {
    content,
    filename,
  });
  return saved ? 'saved' : 'cancelled';
}

export function historyCsv(entries: HistoryEntry[]): string {
  const cell = (value: unknown) => {
    let text = String(value ?? '');
    // Spreadsheet apps treat these leading characters as formulas.
    const trimmed = text.trimStart();
    if (/^[=+\-@\t\r]/.test(text) || /^[=+\-@]/.test(trimmed)) {
      text = `'${text}`;
    }
    return `"${text.replace(/"/g, '""')}"`;
  };
  const rows = [
    ['timestamp', 'text', 'provider', 'mode', 'duration_ms', 'language', 'model']
      .map(cell)
      .join(','),
  ];
  for (const entry of entries) {
    rows.push(
      [
        entry.timestamp,
        entry.text,
        entry.provider,
        entry.mode,
        entry.duration_ms,
        entry.language ?? '',
        entry.model ?? '',
      ]
        .map(cell)
        .join(','),
    );
  }
  return `${rows.join('\r\n')}\r\n`;
}

export function historyTxt(entries: HistoryEntry[]): string {
  return `${entries.map((entry) => `${entry.timestamp}\n${entry.text}`).join('\n\n')}\n`;
}

/**
 * Fetch every page for export. Newest-first OFFSET paging can repeat a row
 * when a dictation is saved mid-export, so entries are de-duplicated by id.
 */
export async function collectHistoryForExport(
  fetchPage: (page: number) => Promise<HistoryEntry[]>,
  maxPages = 10_000,
): Promise<HistoryEntry[]> {
  const all: HistoryEntry[] = [];
  const seen = new Set<string>();
  for (let page = 0; page <= maxPages; page += 1) {
    const batch = await fetchPage(page);
    if (batch.length === 0) break;
    for (const entry of batch) {
      if (seen.has(entry.id)) continue;
      seen.add(entry.id);
      all.push(entry);
    }
  }
  return all;
}

// Local-time filename stamp: YYYYMMDD-HHMMSS. The UTC toISOString form
// landed exports named up to a day off for users far from UTC.
export function localStamp(date = new Date()): string {
  const pad = (n: number) => String(n).padStart(2, '0');
  return (
    `${date.getFullYear()}${pad(date.getMonth() + 1)}${pad(date.getDate())}` +
    `-${pad(date.getHours())}${pad(date.getMinutes())}${pad(date.getSeconds())}`
  );
}
