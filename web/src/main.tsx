import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import './app.css';
import { App } from './App';
import { applyTheme, getStoredTheme } from './lib/theme';

// Apply before first render so light-mode users never see a dark flash
// (the inline dark base in index.html only covers the pre-JS paint).
applyTheme(getStoredTheme());

const root = document.getElementById('root');
if (!root) throw new Error('Missing #root mount node');
createRoot(root).render(
  <StrictMode>
    <App />
  </StrictMode>,
);

