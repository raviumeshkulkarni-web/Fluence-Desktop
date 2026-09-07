// Wizard-local hotkey string builder. Reproduces vanilla wizard.js capture
// logic exactly (modifiers Ctrl/Alt/Shift only in the output; Meta counts as
// a modifier for the key-itself check but is never emitted). Kept local —
// General's builder emits Meta and belongs to the main window's IPC layer.
export function buildWizardHotkey(e: {
  ctrlKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
  key: string;
}): string {
  const parts: string[] = [];
  if (e.ctrlKey) parts.push('Ctrl');
  if (e.altKey) parts.push('Alt');
  if (e.shiftKey) parts.push('Shift');
  const mods = new Set(['Control', 'Alt', 'Shift', 'Meta']);
  if (!mods.has(e.key)) {
    parts.push(e.key === ' ' ? 'Space' : e.key.length === 1 ? e.key.toUpperCase() : e.key);
  }
  return parts.join('+');
}

export const WIZARD_MODIFIER_TOKENS = new Set(['Ctrl', 'Alt', 'Shift', 'Meta']);

export const WIZARD_DEFAULT_HOTKEY = 'Ctrl+Shift+Space';
