import { invokeCmd } from '@/ipc/tauri';

export interface Snippet {
  id: string;
  trigger: string;
  expansion: string;
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

export const deleteSnippet = (id: string) =>
  invokeCmd<void>('delete_snippet', { id });
