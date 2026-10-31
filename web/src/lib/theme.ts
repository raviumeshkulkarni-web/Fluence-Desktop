import { useCallback, useEffect, useState } from 'react';

export type Theme = 'dark' | 'light';

// Explicit choice the user makes. 'auto' follows the OS color scheme live:
// OS dark → app dark, OS light → app light, switching without a restart.
export type ThemeChoice = Theme | 'auto';

export const ACCENT_THEMES = [
  { value: 'fluence', label: 'Fluence Original', dark: '#0BD6E3', light: '#0E7490' },
  { value: 'orange', label: 'Orange', dark: '#FFB45E', light: '#C2410C' },
  { value: 'olive', label: 'Olive Green', dark: '#B7D36B', light: '#5C7C00' },
  { value: 'yellow', label: 'Yellow', dark: '#FFD166', light: '#946A00' },
  { value: 'rose', label: 'Rose', dark: '#FF9FB7', light: '#C1164A' },
  { value: 'monochrome', label: 'Monochrome', dark: '#FFFFFF', light: '#27272A' },
] as const;

export type AccentTheme = (typeof ACCENT_THEMES)[number]['value'];

const THEME_KEY = 'fluence_theme';
const ACCENT_THEME_KEY = 'fluence_accent_theme';

function isAccentTheme(value: unknown): value is AccentTheme {
  return ACCENT_THEMES.some((theme) => theme.value === value);
}

export function getStoredAccentTheme(): AccentTheme {
  try {
    const raw = window.localStorage.getItem(ACCENT_THEME_KEY);
    if (isAccentTheme(raw)) return raw;
  } catch {
    // Storage unavailable (private mode, etc.) — fall back to Fluence Original.
  }
  return 'fluence';
}

export function applyAccentTheme(accentTheme: AccentTheme): void {
  const preset = ACCENT_THEMES.find((theme) => theme.value === accentTheme) ?? ACCENT_THEMES[0];
  const root = document.documentElement;
  root.dataset.accent = accentTheme;
  root.style.setProperty('--fluence-accent-dark', preset.dark);
  root.style.setProperty('--fluence-accent-dark-rgb', hexToRgb(preset.dark));
  root.style.setProperty('--fluence-accent-light', preset.light);
  root.style.setProperty('--fluence-accent-light-rgb', hexToRgb(preset.light));
}

function hexToRgb(hex: string): string {
  const value = Number.parseInt(hex.slice(1), 16);
  return `${(value >> 16) & 255}, ${(value >> 8) & 255}, ${value & 255}`;
}

export function getAccentColor(accentTheme: AccentTheme, theme: Theme): string {
  const preset = ACCENT_THEMES.find((item) => item.value === accentTheme) ?? ACCENT_THEMES[0];
  return theme === 'dark' ? preset.dark : preset.light;
}

export function useAccentTheme(): {
  accentTheme: AccentTheme;
  setAccentTheme: (accentTheme: AccentTheme) => void;
} {
  const [accentTheme, setAccentThemeState] = useState<AccentTheme>(getStoredAccentTheme);

  useEffect(() => {
    applyAccentTheme(accentTheme);
    try {
      window.localStorage.setItem(ACCENT_THEME_KEY, accentTheme);
    } catch {
      // Persistence is an enhancement; the shell still works without it.
    }
  }, [accentTheme]);

  const setAccentTheme = useCallback((next: AccentTheme) => {
    setAccentThemeState(next);
  }, []);

  return { accentTheme, setAccentTheme };
}

function isThemeChoice(value: unknown): value is ThemeChoice {
  return value === 'dark' || value === 'light' || value === 'auto';
}

function systemPrefersLight(): boolean {
  try {
    return !!window.matchMedia?.('(prefers-color-scheme: light)').matches;
  } catch {
    return false;
  }
}

// Default dark preserves existing behavior for current installs.
export function getStoredThemeChoice(): ThemeChoice {
  try {
    const raw = window.localStorage.getItem(THEME_KEY);
    if (isThemeChoice(raw)) return raw;
  } catch {
    // Storage unavailable (private mode, etc.) — fall back to dark.
  }
  return 'dark';
}

// Back-compat: the effective theme for a stored value (pre-paint boot path).
export function getStoredTheme(): Theme {
  return resolveThemeChoice(getStoredThemeChoice());
}

export function resolveThemeChoice(choice: ThemeChoice): Theme {
  if (choice === 'light') return 'light';
  if (choice === 'dark') return 'dark';
  return systemPrefersLight() ? 'light' : 'dark';
}

// Single place that touches the DOM. Overlay and wizard windows never call
// this, so they stay dark regardless of the settings-shell choice.
export function applyTheme(theme: Theme): void {
  document.documentElement.dataset.theme = theme;
  document.documentElement.style.colorScheme = theme;
}

const CYCLE: ThemeChoice[] = ['dark', 'light', 'auto'];

export function cycleThemeChoice(choice: ThemeChoice): ThemeChoice {
  return CYCLE[(CYCLE.indexOf(choice) + 1 + CYCLE.length) % CYCLE.length] ?? 'dark';
}

export function useTheme(): {
  theme: Theme;
  choice: ThemeChoice;
  setThemeChoice: (choice: ThemeChoice) => void;
  cycleTheme: () => void;
  toggleTheme: () => void;
} {
  const [choice, setChoice] = useState<ThemeChoice>(getStoredThemeChoice);
  // Bumps whenever the OS scheme flips so 'auto' re-resolves live.
  const [systemLight, setSystemLight] = useState<boolean>(systemPrefersLight);

  useEffect(() => {
    let mq: MediaQueryList | null = null;
    const onChange = (e: MediaQueryListEvent) => setSystemLight(e.matches);
    try {
      mq = window.matchMedia?.('(prefers-color-scheme: light)') ?? null;
      mq?.addEventListener('change', onChange);
    } catch {
      mq = null;
    }
    return () => {
      try {
        mq?.removeEventListener('change', onChange);
      } catch {
        // Listener removal is best-effort on teardown.
      }
    };
  }, []);

  const theme: Theme = choice === 'auto' ? (systemLight ? 'light' : 'dark') : choice;

  useEffect(() => {
    applyTheme(theme);
    try {
      window.localStorage.setItem(THEME_KEY, choice);
    } catch {
      // Persistence is an enhancement; the shell still works without it.
    }
  }, [theme, choice]);

  const setThemeChoice = useCallback((next: ThemeChoice) => {
    setChoice(next);
  }, []);

  const cycleTheme = useCallback(() => {
    setChoice((c) => cycleThemeChoice(c));
  }, []);

  // Quick toggle (sidebar-previous behavior, palette, Ctrl+Shift+L): flips
  // explicit themes; from 'auto' it pins the opposite of what's showing.
  const toggleTheme = useCallback(() => {
    setChoice((c) => {
      if (c === 'auto') return resolveThemeChoice('auto') === 'dark' ? 'light' : 'dark';
      return c === 'dark' ? 'light' : 'dark';
    });
  }, []);

  return { theme, choice, setThemeChoice, cycleTheme, toggleTheme };
}
