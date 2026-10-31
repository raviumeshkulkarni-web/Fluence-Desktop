import {
  getUiLanguage,
  normalizeLanguageTag,
  resolveSystemLanguage,
  SYSTEM_LANGUAGE,
  UI_LANGUAGES,
  type UiLanguage,
} from './languages';
import type { TranslationKey } from './keys';

export {
  UI_LANGUAGES,
  SYSTEM_LANGUAGE,
  getUiLanguage,
  normalizeLanguageTag,
  resolveSystemLanguage,
};
export type { UiLanguage, TranslationKey };

export const LANGUAGE_STORAGE_KEY = 'fluence_ui_language';

type Catalog = Record<string, string>;

let preference = SYSTEM_LANGUAGE;
let locale = 'en';
let catalog: Catalog = {};
let initialized = false;

function readStoredPreference(): string | null {
  try {
    return window.localStorage.getItem(LANGUAGE_STORAGE_KEY);
  } catch {
    return null;
  }
}

export function storeLanguagePreference(code: string): void {
  try {
    window.localStorage.setItem(LANGUAGE_STORAGE_KEY, code);
  } catch {
  }
}

export function getLanguagePreference(): string {
  const stored = readStoredPreference();
  if (!stored || stored === SYSTEM_LANGUAGE) return SYSTEM_LANGUAGE;
  return getUiLanguage(stored)?.code ?? SYSTEM_LANGUAGE;
}

export function getLocale(): string {
  return locale;
}

export function getLanguageState(): { preference: string; locale: string } {
  return { preference, locale };
}

function resolvePreference(preference: string): string {
  if (preference === SYSTEM_LANGUAGE) return resolveSystemLanguage();
  return normalizeLanguageTag(preference) ?? resolveSystemLanguage();
}

async function loadCatalog(code: string): Promise<Catalog> {
  if (code === 'en') return {};

  const url = new URL(`i18n/${code}.json`, document.baseURI);
  const response = await fetch(url);
  if (!response.ok) {
    throw new Error(`i18n catalog ${code} unavailable (${response.status})`);
  }

  const data = (await response.json()) as unknown;
  if (!data || typeof data !== 'object' || Array.isArray(data)) {
    throw new Error(`i18n catalog ${code} is malformed`);
  }

  const parsed: Catalog = {};
  for (const [key, value] of Object.entries(data as Record<string, unknown>)) {
    if (typeof value === 'string') parsed[key] = value;
  }
  return parsed;
}

export async function initI18n(): Promise<string> {
  if (initialized) return locale;
  initialized = true;

  preference = getLanguagePreference();
  const target = resolvePreference(preference);
  if (target !== 'en') {
    try {
      catalog = await loadCatalog(target);
      locale = target;
    } catch (err) {
      console.warn('[i18n] falling back to English:', err);
    }
  }

  document.documentElement.lang = locale;
  return locale;
}

function interpolate(template: string, params?: Record<string, string | number>): string {
  if (!params) return template;
  return template.replace(/\{(\w+)\}/g, (match, name: string) => {
    const value = params[name];
    return value === undefined ? match : String(value);
  });
}

export function t(key: TranslationKey, params?: Record<string, string | number>): string {
  const translated = Object.prototype.hasOwnProperty.call(catalog, key) ? catalog[key] : key;
  return interpolate(translated, params);
}

export function tr(message: string): string {
  if (!message) return message;
  if (Object.prototype.hasOwnProperty.call(catalog, message)) return catalog[message];

  const normalized = message.replace(/\s+/g, ' ').trim();
  return Object.prototype.hasOwnProperty.call(catalog, normalized)
    ? catalog[normalized]
    : message;
}

export function reloadWithLanguage(code: string): void {
  storeLanguagePreference(code);
  window.setTimeout(() => window.location.reload(), 120);
}
