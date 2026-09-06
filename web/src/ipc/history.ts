import { invokeCmd, listenEvent } from '@/ipc/tauri';
import { getSuggestions } from '@/ipc/dictionary';

export interface HistoryEntry {
  id: string;
  timestamp: string;
  text: string;
  mode: string;
  duration_ms: number;
  provider: string;
  char_count: number;
  model?: string | null;
  language?: string | null;
}

// Command names + argument shapes reproduced exactly from vanilla
// (loadHistory/deleteHistoryItem/setupHistory-clear/copyHistoryItem).
export const getHistory = (page: number, searchQuery: string | null) =>
  invokeCmd<HistoryEntry[]>('get_history', { page, searchQuery });

export const deleteHistoryEntry = (id: string) =>
  invokeCmd<void>('delete_history_entry', { id });

export const clearHistory = () => invokeCmd<void>('clear_history');

export const copyText = (text: string) =>
  invokeCmd<void>('copy_text', { text });

export const subscribeHistoryUpdated = (handler: () => void) =>
  listenEvent('history-updated', handler);

// Shell Ctrl+F/K support (vanilla setupKeyboardShortcuts history branch):
// the shell requests focus, the History route consumes it on mount (after
// navigation remounts it) or immediately when already mounted.
let historySearchFocusRequested = false;

export function requestHistorySearchFocus() {
  historySearchFocusRequested = true;
  window.dispatchEvent(new CustomEvent('fluence:focus-history-search'));
}

export function consumeHistorySearchFocus(): boolean {
  const requested = historySearchFocusRequested;
  historySearchFocusRequested = false;
  return requested;
}

// Candidate-word markers (vanilla loadPendingSuggestionMap): pending
// auto-learn suggestions keyed by lowercase spoken form, cached 30s so
// history paging stays cheap. First entry wins per spoken form.
export interface PendingMark {
  id: string;
  corrected: string;
}

let pendingMap: Map<string, PendingMark> | null = null;
let pendingFetchedAt = 0;

export async function loadPendingSuggestionMap(
  force = false,
): Promise<Map<string, PendingMark>> {
  const now = Date.now();
  if (!force && pendingMap && now - pendingFetchedAt < 30000) {
    return pendingMap;
  }
  pendingMap = new Map();
  pendingFetchedAt = now;
  try {
    const suggestions = await getSuggestions();
    suggestions.forEach((s) => {
      const spoken = s.spoken?.trim();
      if (s.status !== 'pending' || !spoken) return;
      const key = spoken.toLowerCase();
      if (!pendingMap!.has(key)) {
        pendingMap!.set(key, { id: s.id, corrected: s.corrected });
      }
    });
  } catch (err) {
    console.error('Failed to load suggestions for markers:', err);
  }
  return pendingMap;
}
