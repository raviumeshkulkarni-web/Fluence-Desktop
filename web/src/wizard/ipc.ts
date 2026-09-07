import { invokeCmd } from '@/ipc/tauri';

// Wizard IPC wrappers. Command names + argument shapes reproduced exactly
// from vanilla src/js/wizard.js — no new commands, no reshaping.

export const minimizeWizard = () => invokeCmd<void>('minimize_wizard');

export const closeWizard = () => invokeCmd<void>('close_wizard');

export const showMainWindow = () => invokeCmd<void>('show_main_window');

export const saveApiKey = (target: string, key: string) =>
  invokeCmd<void>('save_api_key', { target, key });

export const getApiKey = (target: string) =>
  invokeCmd<string>('get_api_key', { target });

export const fetchSttModels = (baseUrl: string, apiKey: string, keep: string | null) =>
  invokeCmd<{ models: string[] }>('fetch_stt_models', { baseUrl, apiKey, keep });

export const fetchModels = (baseUrl: string, apiKey: string) =>
  invokeCmd<string[]>('fetch_models', { baseUrl, apiKey });

export const testSttConnection = (baseUrl: string, apiKey: string) =>
  invokeCmd<string>('test_stt_connection', { baseUrl, apiKey });

export const startRecording = (deviceId: string | null) =>
  invokeCmd<void>('start_recording', { deviceId });

export const stopRecording = () => invokeCmd<string>('stop_recording');

export interface TranscribeReq {
  base_url: string;
  api_key: string;
  model: string;
  wav_b64: string;
  language: string;
}

export const transcribeAudio = (req: TranscribeReq) =>
  invokeCmd<string>('transcribe_audio', { req });

export interface WizardSettings {
  hotkey: string;
  recording_mode: string;
  overlay_position: string;
  stt_provider: { preset: string; base_url: string; model: string; api_key_saved: boolean };
  llm_provider: { preset: string; base_url: string; model: string; api_key_saved: boolean };
  auto_start: boolean;
  sound_on_complete: boolean;
  agent_mode_threshold_ms: number;
  language: string;
  agent_hotkey: string;
  agent_recording_mode: string;
  ai_polish_style: string;
  auto_grab_highlight: boolean;
  audio_device_id: string | null;
  theme: string;
  first_run: boolean;
}

export const updateSettings = (settings: WizardSettings) =>
  invokeCmd<void>('update_settings', { settings });

export const updateHotkeys = (args: {
  transcriptionShortcut: string;
  transcriptionMode: string;
  agentShortcut: string;
  agentMode: string;
}) => invokeCmd<void>('update_hotkeys', args);
