export interface UiLanguage {
  code: string;
  nativeName: string;
}

export const SYSTEM_LANGUAGE = 'system';

export const UI_LANGUAGES: UiLanguage[] = [
  { code: 'en', nativeName: 'English' },
  { code: 'de', nativeName: 'Deutsch' },
];

const BY_CODE = new Map(UI_LANGUAGES.map((language) => [language.code, language]));

export function getUiLanguage(code: string | null | undefined): UiLanguage | undefined {
  return code ? BY_CODE.get(code.toLowerCase()) : undefined;
}

export function normalizeLanguageTag(tag: string | null | undefined): string | null {
  if (!tag) return null;
  const base = tag.trim().replace(/_/g, '-').split('-')[0].toLowerCase();
  return BY_CODE.has(base) ? base : null;
}

export function resolveSystemLanguage(): string {
  const primary = navigator.languages?.[0] ?? navigator.language;
  return normalizeLanguageTag(primary) ?? 'en';
}
