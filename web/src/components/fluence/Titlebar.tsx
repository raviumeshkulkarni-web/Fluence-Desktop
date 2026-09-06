import { useState } from 'react';
import {
  hideMainWindow,
  minimizeMainWindow,
  toggleMaximizeMainWindow,
} from '@/ipc/tauri';

// Frameless window controls. Behavior mirrors vanilla setupTitlebar exactly:
// minimize minimizes, maximize toggles (icon follows the backend result),
// close HIDES to tray (the app keeps running).
export function Titlebar() {
  const [maximized, setMaximized] = useState(false);

  const onMaximize = async () => {
    try {
      setMaximized(await toggleMaximizeMainWindow());
    } catch {
      /* window controls are unavailable outside Tauri — stay put */
    }
  };

  return (
    <div data-tauri-drag-region className="titlebar">
      <div className="titlebar-title" data-tauri-drag-region>
        Fluence Settings
      </div>
      <div className="titlebar-controls">
        <button
          type="button"
          className="titlebar-button"
          id="titlebar-minimize"
          aria-label="Minimize window"
          onClick={() => void minimizeMainWindow().catch(() => undefined)}
        >
          <svg width="11" height="1" viewBox="0 0 11 1" aria-hidden="true">
            <rect width="11" height="1" fill="currentColor" />
          </svg>
        </button>
        <button
          type="button"
          className="titlebar-button"
          id="titlebar-maximize"
          aria-label={maximized ? 'Restore window' : 'Maximize window'}
          title={maximized ? 'Restore window' : 'Maximize window'}
          onClick={() => void onMaximize()}
        >
          {maximized ? (
            <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true">
              <rect x="2.25" y="0.75" width="7" height="7" rx="1" fill="none" stroke="currentColor" strokeWidth="1.2" />
              <rect x="0.75" y="2.75" width="6.5" height="6.5" rx="1" fill="none" stroke="currentColor" strokeWidth="1.2" />
            </svg>
          ) : (
            <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true">
              <rect x="0.75" y="0.75" width="8.5" height="8.5" rx="1" fill="none" stroke="currentColor" strokeWidth="1.2" />
            </svg>
          )}
        </button>
        <button
          type="button"
          className="titlebar-button titlebar-close"
          id="titlebar-close"
          aria-label="Hide to tray"
          title="Hide to tray"
          onClick={() => void hideMainWindow().catch(() => undefined)}
        >
          <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true">
            <path d="M5 1v5M2 4.5L5 7.5l3-3M1 9h8" stroke="currentColor" strokeWidth="1.2" strokeLinecap="round" strokeLinejoin="round" />
          </svg>
        </button>
      </div>
    </div>
  );
}
