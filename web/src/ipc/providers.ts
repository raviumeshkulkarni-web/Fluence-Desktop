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

// Credential-store target shape reproduced exactly from vanilla
// (setupProviderCards): `Fluence/STT_ApiKey/<preset>` with spaces folded
// to underscores, lowercased — e.g. "Local Offline" → "local_offline".
export function keyTarget(kind: ProviderKind, preset: string): string {
  const base = kind === 'stt' ? 'Fluence/STT_ApiKey' : 'Fluence/LLM_ApiKey';
  return `${base}/${preset.toLowerCase().replace(/ /g, '_')}`;
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
