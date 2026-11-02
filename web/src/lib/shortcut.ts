import { tr } from '@/i18n';

export function formatShortcut(value: string): string {
  return value.replace(
    /\b(Ctrl|Shift|Alt|Space|Enter|Escape|Tab|Backspace|Delete|Win|Meta)\b/g,
    (token) => tr(token),
  );
}
