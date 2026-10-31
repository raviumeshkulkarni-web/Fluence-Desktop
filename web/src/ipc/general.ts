import { invokeCmd } from '@/ipc/tauri';
import {
  getCachedSettings,
  persist,
  registerPersistHook,
} from '@/ipc/settings';
import { toast } from '@/components/fluence/Toasts';

// Command names + argument shapes reproduced exactly from vanilla
// (setupHotkeyRecorders/applyHotkeyChanges/applyAutostartChange,
// populateAudioDevices). Note the camelCase update_hotkeys args:
// verbatim as the vanilla frontend sends them.
export const updateHotkeys = (args: {
  transcriptionShortcut: string;
  transcriptionMode: string;
  agentShortcut: string;
  agentMode: string;
}) => invokeCmd<void>('update_hotkeys', args);

export const setAutostart = (enabled: boolean) =>
  invokeCmd<void>('set_autostart', { enabled });

export const listAudioDevices = () =>
  invokeCmd<string[]>('list_audio_devices');

export interface HotkeyDesired {
  transcriptionShortcut: string;
  transcriptionMode: string;
  agentShortcut: string;
  agentMode: string;
}

// Vanilla module-level dedupe state (lastAppliedHotkeys /
// lastAppliedAutostart): re-registering identical hotkeys or re-applying
// an unchanged autostart is skipped.
let lastAppliedHotkeys: HotkeyDesired | null = null;
let lastAppliedAutostart = false;

function desiredHotkeys(): HotkeyDesired {
  const s = getCachedSettings();
  return {
    transcriptionShortcut: (s?.hotkey as string) ?? '',
    transcriptionMode: (s?.recording_mode as string) ?? '',
    agentShortcut: (s?.agent_hotkey as string) ?? '',
    agentMode: (s?.agent_recording_mode as string) ?? '',
  };
}

async function applyHotkeyChanges(): Promise<void> {
  const desired = desiredHotkeys();
  if (
    lastAppliedHotkeys &&
    JSON.stringify(lastAppliedHotkeys) === JSON.stringify(desired)
  ) {
    return;
  }
  lastAppliedHotkeys = desired;
  try {
    await updateHotkeys(desired);
  } catch (err) {
    toast('Failed to update hotkeys: ' + String(err), 'error');
  }
}

async function applyAutostartChange(): Promise<void> {
  const autoStart = (getCachedSettings()?.auto_start as boolean) ?? false;
  if (lastAppliedAutostart === autoStart) return;
  lastAppliedAutostart = autoStart;
  try {
    await setAutostart(autoStart);
  } catch (err) {
    console.error('Failed to apply autostart:', err);
  }
}

/** Mirrors vanilla setupAutoApply's autostart baseline capture. */
export function initGeneralApply() {
  lastAppliedAutostart =
    (getCachedSettings()?.auto_start as boolean) ?? false;
}

registerPersistHook('hotkeys', applyHotkeyChanges);
registerPersistHook('autostart', applyAutostartChange);

/** Queue a general-settings write with vanilla flush features. */
export function persistGeneral(...features: string[]) {
  persist('general', ...features);
}

// Shared hotkey-recorder activity flag. Vanilla registers its capture
// listeners before the global Esc handler and consumes Esc via
// stopImmediatePropagation; the shell checks this flag for the same
// outcome without depending on listener order.
let hotkeyRecording = false;

export function setHotkeyRecording(active: boolean) {
  hotkeyRecording = active;
}

export function isHotkeyRecording(): boolean {
  return hotkeyRecording;
}

/** Vanilla buildHotkeyString: modifiers first, Space/single-char fixups. */
export function buildHotkeyString(e: {
  ctrlKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
  metaKey: boolean;
  key: string;
}): string {
  const parts: string[] = [];
  if (e.ctrlKey) parts.push('Ctrl');
  if (e.altKey) parts.push('Alt');
  if (e.shiftKey) parts.push('Shift');
  if (e.metaKey) parts.push('Meta');
  if (!MODIFIER_KEYS.has(e.key)) {
    parts.push(
      e.key === ' ' ? 'Space' : e.key.length === 1 ? e.key.toUpperCase() : e.key,
    );
  }
  return parts.join('+');
}

export const MODIFIER_KEYS = new Set(['Control', 'Alt', 'Shift', 'Meta']);

export const DEFAULT_HOTKEY = 'Ctrl+Shift+Space';
export const DEFAULT_AGENT_HOTKEY = 'Ctrl+Shift+A';
