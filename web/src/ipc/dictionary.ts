import { invokeCmd } from '@/ipc/tauri';

export interface DictEntry {
  id: string;
  spoken: string;
  corrected: string;
}

export interface Suggestion {
  id: string;
  spoken: string;
  corrected: string;
  frequency: number;
  // Review-lifecycle status ("pending" | "accepted" | "dismissed").
  // Only read by the History candidate-word markers; the Dictionary
  // route never filters on it.
  status?: string;
}

interface AutoAccepted {
  spoken: string;
  corrected: string;
}

// Command names + argument shapes reproduced exactly from vanilla.
export const getDictionary = () =>
  invokeCmd<DictEntry[]>('get_dictionary');

export const addDictionaryEntry = (spoken: string, corrected: string) =>
  invokeCmd<DictEntry>('add_dictionary_entry', { spoken, corrected });

export const deleteDictionaryEntry = (id: string) =>
  invokeCmd<void>('delete_dictionary_entry', { id });

export const getAutoAccepted = () =>
  invokeCmd<AutoAccepted[]>('get_auto_accepted_suggestions');

export const getSuggestions = () =>
  invokeCmd<Suggestion[]>('get_suggestions');

export const acceptSuggestion = (id: string) =>
  invokeCmd<void>('accept_suggestion_command', { id });

export const dismissSuggestion = (id: string) =>
  invokeCmd<void>('dismiss_suggestion_command', { id });

export const expireStaleSuggestions = () =>
  invokeCmd<void>('expire_stale_suggestions_command');

export const exportDictionaryJson = () =>
  invokeCmd<string>('export_dictionary');

async function importDictionaryJson(jsonData: string) {
  return invokeCmd<number>('import_dictionary', { jsonData });
}

interface DialogPlugin {
  open: (opts: unknown) => Promise<string | string[] | null>;
}

interface FsPlugin {
  readTextFile: (path: string) => Promise<string>;
}

function plugins(): { dialog: DialogPlugin; fs: FsPlugin } {
  const w = window as unknown as Record<string, unknown>;
  const dialog = w.__TAURI_PLUGIN_DIALOG__ as DialogPlugin | undefined;
  const fs = w.__TAURI_PLUGIN_FS__ as FsPlugin | undefined;
  if (!dialog || !fs) throw new Error('File dialog plugin not available');
  return { dialog, fs };
}

/** Vanilla import flow: JSON file picker → backend import → count. */
export async function importDictionaryFile(): Promise<number | null> {
  const { dialog, fs } = plugins();
  const path = await dialog.open({
    filters: [{ name: 'JSON', extensions: ['json'] }],
  });
  if (!path) return null;
  const json = await fs.readTextFile(Array.isArray(path) ? path[0] : path);
  return importDictionaryJson(json);
}

/** Vanilla export flow: backend JSON → browser download. */
export function downloadDictionaryJson(json: string) {
  const blob = new Blob([json], { type: 'application/json' });
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = 'fluence-dictionary.json';
  a.click();
  URL.revokeObjectURL(url);
}
