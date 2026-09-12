import { useCallback, useEffect, useState } from 'react';

export type Theme = 'dark' | 'light';

const THEME_KEY = 'fluence_theme';

function isTheme(value: unknown): value is Theme {
  return value === 'dark' || value === 'light';
}

// Default dark preserves existing behavior for current installs.
export function getStoredTheme(): Theme {
  try {
    const raw = window.localStorage.getItem(THEME_KEY);
    if (isTheme(raw)) return raw;
  } catch {
    // Storage unavailable (private mode, etc.) — fall back to dark.
  }
  return 'dark';
}

// Single place that touches the DOM. Overlay and wizard windows never call
// this, so they stay dark regardless of the settings-shell choice.
export function applyTheme(theme: Theme): void {
  document.documentElement.dataset.theme = theme;
  document.documentElement.style.colorScheme = theme;
}

export function useTheme(): { theme: Theme; toggleTheme: () => void } {
  const [theme, setTheme] = useState<Theme>(getStoredTheme);

  useEffect(() => {
    applyTheme(theme);
    try {
      window.localStorage.setItem(THEME_KEY, theme);
    } catch {
      // Persistence is an enhancement; the shell still works without it.
    }
  }, [theme]);

  const toggleTheme = useCallback(() => {
    setTheme((t) => (t === 'dark' ? 'light' : 'dark'));
  }, []);

  return { theme, toggleTheme };
}
