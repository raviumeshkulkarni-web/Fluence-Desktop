import { useCallback, useEffect, useState } from 'react';

export type Theme = 'dark' | 'light';

// Explicit choice the user makes. 'auto' follows the OS color scheme live:
// OS dark → app dark, OS light → app light, switching without a restart.
export type ThemeChoice = Theme | 'auto';

const THEME_KEY = 'fluence_theme';

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
