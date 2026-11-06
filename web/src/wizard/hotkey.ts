// Wizard-local hotkey string builder. Mirrors the main window's builder,
// including the Windows key (Meta). Kept local so the wizard bundle does not
// pull in the settings IPC layer.
export function buildWizardHotkey(e: {
  ctrlKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
  metaKey: boolean;
  key: string;
  code?: string;
}): string {
  const parts: string[] = [];
  if (e.ctrlKey) parts.push('Ctrl');
  if (e.altKey) parts.push('Alt');
  if (e.shiftKey) parts.push('Shift');
  if (e.metaKey) parts.push('Meta');
  const mods = new Set(['Control', 'Alt', 'Shift', 'Meta']);
  if (!mods.has(e.key)) {
    // The main plus key is Shift+'='; numpad plus has its own backend token.
    const key =
      e.code === 'NumpadAdd' ? 'NumpadAdd' : e.key === '+' ? '=' : e.key;
    parts.push(key === ' ' ? 'Space' : key.length === 1 ? key.toUpperCase() : key);
  }
  return parts.join('+');
}

export const WIZARD_MODIFIER_TOKENS = new Set(['Ctrl', 'Alt', 'Shift', 'Meta']);

export const WIZARD_DEFAULT_HOTKEY = 'Ctrl+Shift+Space';

// The backend speaks "Meta" and "NumpadAdd"; show users familiar labels.
export function displayHotkey(hotkey: string): string {
  return hotkey.replace(/\bMeta\b/g, 'Win').replace(/\bNumpadAdd\b/g, 'NumPlus');
}

// A global shortcut must not steal plain typing. Require at least one
// modifier, except for function keys (F1–F24), which are safe on their own.
export function isAllowedWizardHotkey(hotkey: string): boolean {
  const parts = hotkey.split('+').filter(Boolean);
  if (parts.length === 0) return false;
  const key = parts[parts.length - 1];
  if (WIZARD_MODIFIER_TOKENS.has(key)) return false;
  if (parts.slice(0, -1).some((p) => WIZARD_MODIFIER_TOKENS.has(p))) return true;
  return /^F([1-9]|1[0-9]|2[0-4])$/.test(key);
}
