import { invokeCmd, listenEvent } from '@/ipc/tauri';

export type ProviderKind = 'stt' | 'llm';

export interface ProviderConfig {
  preset: string;
  base_url: string;
  model: string;
  api_key_saved: boolean;
}

export interface SttModelList {
  models: string[];
  filtered: boolean;
}

export interface OfflineDownloadProgress {
  progress: number;
  status: string;
  currentFile?: string;
  bytesDownloaded?: number;
  totalBytes?: number;
  errorMessage?: string;
}

// Canonical preset slug — byte-for-byte parity with backend `sanitize_preset`
// (src-tauri/src/credentials.rs, FIX-02 contract). Rules, in order:
// lowercase, replace every ASCII space with `_`, then map any char outside
// `[a-z0-9_]` to `_`. Collision policy (documented backend-side): slugs that
// differ only by mapped characters share one slot (e.g. `my-provider` and
// `my_provider` both resolve to `my_provider`; last write wins). Built-in
// presets (`groq`, `openai`, `mistral`, `custom`, `Local Offline`) are
// collision-free and resolve exactly as before (`Local Offline` → `local_offline`).
export function canonicalPresetSlug(preset: string): string {
  return String(preset || '')
    .toLowerCase()
    .replace(/ /g, '_')
    .replace(/[^a-z0-9_]/g, '_');
}

// Credential-store target shape: `Fluence/STT_ApiKey/<slug>` /
// `Fluence/LLM_ApiKey/<slug>` using the canonical slug above, so save, read,
// delete, and the backend's secure server-side lookup always name one slot.
export function keyTarget(kind: ProviderKind, preset: string): string {
  const base = kind === 'stt' ? 'Fluence/STT_ApiKey' : 'Fluence/LLM_ApiKey';
  return `${base}/${canonicalPresetSlug(preset)}`;
}

// Command names + argument shapes reproduced exactly from vanilla.
// Note the camelCase args on the model/test commands ({ baseUrl, apiKey }):
// verbatim as the vanilla frontend sends them.
export const getApiKey = (target: string) =>
  invokeCmd<string>('get_api_key', { target });

export const saveApiKey = (target: string, key: string) =>
  invokeCmd<void>('save_api_key', { target, key });

export const fetchModels = (baseUrl: string, apiKey: string) =>
  invokeCmd<string[]>('fetch_models', { baseUrl, apiKey });

export const fetchSttModels = (
  baseUrl: string,
  apiKey: string,
  keep: string | null,
) =>
  invokeCmd<SttModelList>('fetch_stt_models', { baseUrl, apiKey, keep });

export const testSttConnection = (baseUrl: string, apiKey: string) =>
  invokeCmd<string>('test_stt_connection', { baseUrl, apiKey });

export const testLlmConnection = (
  baseUrl: string,
  apiKey: string,
  model: string,
) => invokeCmd<string>('test_llm_connection', { baseUrl, apiKey, model });

export const getOfflineModelStatus = () =>
  invokeCmd<boolean>('get_offline_model_status');

export const getMoonshineV2SmallModelStatus = () =>
  invokeCmd<boolean>('get_moonshine_v2_small_model_status');

export const getMoonshineV2MediumModelStatus = () =>
  invokeCmd<boolean>('get_moonshine_v2_medium_model_status');

export const downloadOfflineModel = () =>
  invokeCmd<void>('download_offline_model');

export const downloadMoonshineV2SmallModel = () =>
  invokeCmd<void>('download_moonshine_v2_small_model');

export const downloadMoonshineV2MediumModel = () =>
  invokeCmd<void>('download_moonshine_v2_medium_model');

export const deleteOfflineModel = () =>
  invokeCmd<number>('delete_offline_model');

export const deleteMoonshineV2SmallModel = () =>
  invokeCmd<number>('delete_moonshine_v2_small_model');

export const deleteMoonshineV2MediumModel = () =>
  invokeCmd<number>('delete_moonshine_v2_medium_model');

export const cancelOfflineDownload = () =>
  invokeCmd<void>('cancel_offline_download');

export const subscribeOfflineDownloadProgress = (
  handler: (payload: OfflineDownloadProgress) => void,
) => listenEvent<OfflineDownloadProgress>('offline-download-progress', handler);
