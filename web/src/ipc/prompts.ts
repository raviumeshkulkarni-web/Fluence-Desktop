import { invokeCmd } from '@/ipc/tauri';
import type { ClaimOutcome } from '@/lib/claim';

export interface CustomPromptStyle {
  id: string;
  name: string;
  hint: string;
  /** True only for an unclaimed pre-account style while an account is signed in. Backend-computed; see CustomAgent.claimable. */
  claimable?: boolean;
}

export interface BuiltinPromptStyle {
  id: string;
  title: string;
  description: string;
}

export interface PromptsView {
  builtin_styles: BuiltinPromptStyle[];
  custom_styles: CustomPromptStyle[];
  overrides: Record<string, string>;
}

export interface InstalledApp {
  exe: string;
  name: string;
  icon_data_url?: string;
}

// Backend: prompts.rs + installed_apps.rs.
export const getPrompts = () => invokeCmd<PromptsView>('get_prompts');

export const savePromptStyle = (name: string, hint: string, id?: string) =>
  invokeCmd<CustomPromptStyle>('save_prompt_style', {
    name,
    hint,
    id: id ?? null,
  });

export const deletePromptStyle = (id: string) =>
  invokeCmd<string[]>('delete_prompt_style', { id });

// Adopt ONE unclaimed pre-account style into the account signed in right now.
// Only `id` crosses the boundary — the destination account is resolved backend-side
// and is never a parameter. See `claimLegacyAgent` for the full rationale.
export const claimLegacyPromptStyle = (id: string) =>
  invokeCmd<ClaimOutcome>('claim_legacy_prompt_style', { id });

export const setPromptOverride = (exe: string, styleId: string) =>
  // Tauri v2 expects command args in camelCase: the runtime rejects
  // `style_id` with "missing required key styleId".
  invokeCmd<void>('set_prompt_override', { exe, styleId });

export const clearPromptOverride = (exe: string) =>
  invokeCmd<void>('clear_prompt_override', { exe });

export const listInstalledApps = () =>
  invokeCmd<InstalledApp[]>('list_installed_apps');
