import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import './app.css';
import { App } from './App';
import { initI18n } from './i18n';
import { applyAccentTheme, applyTheme, getStoredAccentTheme, getStoredTheme } from './lib/theme';

// Apply before first render so light-mode users never see a dark flash
// (the inline dark base in index.html only covers the pre-JS paint).
applyTheme(getStoredTheme());
applyAccentTheme(getStoredAccentTheme());

async function boot() {
  await initI18n();

  const root = document.getElementById('root');
  if (!root) throw new Error('Missing #root mount node');
  createRoot(root).render(
    <StrictMode>
      <App />
    </StrictMode>,
  );
}

void boot();

