import { invokeCmd } from '@/ipc/tauri';

export interface Snippet {
  id: string;
  trigger: string;
  expansion: string;
  // Per-entry switch; the backend field rides the frozen sync wire as isEnabled.
  is_enabled?: boolean;
}

export interface SnippetStore {
  enabled: boolean;
  snippets: Snippet[];
}

// Command names + argument shapes reproduced exactly from vanilla
// (src/js/settings.js setupSnippets/loadSnippets/saveSnippetEntry).
export const getSnippets = () =>
  invokeCmd<SnippetStore>('get_snippets');

export const setSnippetsEnabled = (enabled: boolean) =>
  invokeCmd<void>('set_snippets_enabled', { enabled });

export const addSnippet = (trigger: string, expansion: string) =>
  invokeCmd<Snippet>('add_snippet', { trigger, expansion });

export const updateSnippet = (id: string, trigger: string, expansion: string) =>
  invokeCmd<void>('update_snippet', { id, trigger, expansion });

export const setSnippetEnabled = (id: string, enabled: boolean) =>
  invokeCmd<void>('set_snippet_enabled', { id, enabled });

export const deleteSnippet = (id: string) =>
  invokeCmd<void>('delete_snippet', { id });
