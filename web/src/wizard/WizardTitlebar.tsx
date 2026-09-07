import { closeWizard, minimizeWizard } from './ipc';

// Wizard frameless controls: minimize + close only (no maximize on the
// 680×540 fixed window). Same inline glyphs as vanilla src/wizard.html.
export function WizardTitlebar() {
  return (
    <div data-tauri-drag-region className="titlebar">
      <div className="titlebar-title" data-tauri-drag-region>
        Welcome to Fluence
      </div>
      <div className="titlebar-controls">
        <button
          type="button"
          className="titlebar-button"
          id="titlebar-minimize"
          aria-label="Minimize window"
          onClick={() => void minimizeWizard().catch(() => undefined)}
        >
          <svg width="11" height="1" viewBox="0 0 11 1" aria-hidden="true">
            <rect width="11" height="1" fill="currentColor" />
          </svg>
        </button>
        <button
          type="button"
          className="titlebar-button titlebar-close"
          id="titlebar-close"
          aria-label="Close window"
          onClick={() => void closeWizard().catch(() => undefined)}
        >
          <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true">
            <path
              d="M1 1l8 8M9 1L1 9"
              stroke="currentColor"
              strokeWidth="1.2"
              strokeLinecap="round"
            />
          </svg>
        </button>
      </div>
    </div>
  );
}
