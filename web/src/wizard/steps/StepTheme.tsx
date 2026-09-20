import { useRef } from 'react';
import type { KeyboardEvent } from 'react';
import type { WizardData } from '../Wizard';

interface StepThemeProps {
  data: WizardData;
  onPatch: (patch: Partial<WizardData>) => void;
}

export type ThemeMode = 'dark' | 'light' | 'auto';

const THEMES: { value: ThemeMode; label: string; desc: string }[] = [
  { value: 'dark', label: 'Dark', desc: 'Easy on the eyes, day and night' },
  { value: 'light', label: 'Light', desc: 'Bright and crisp for daylight' },
  { value: 'auto', label: 'System', desc: 'Follow Windows, switches automatically' },
];

// Step 2: settings-shell theme. The overlay stays dark by design; the wizard
// previews the choice live (see Wizard) and the shell keeps it via StepDone.
// this dresses the main Settings window. Each option previews itself as a
// miniature window mock; System resolves once at finish time (see StepDone).
export function StepTheme({ data, onPatch }: StepThemeProps) {
  const groupRef = useRef<HTMLDivElement>(null);

  const onGroupKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    const buttons = Array.from(
      groupRef.current?.querySelectorAll<HTMLButtonElement>('.theme-option') ?? [],
    );
    if (!buttons.length) return;
    const current = buttons.indexOf(document.activeElement as HTMLButtonElement);
    let next: number | null = null;
    if (e.key === 'ArrowRight' || e.key === 'ArrowDown') {
      e.preventDefault();
      next = (current + 1 + buttons.length) % buttons.length;
    } else if (e.key === 'ArrowLeft' || e.key === 'ArrowUp') {
      e.preventDefault();
      next = (current - 1 + buttons.length) % buttons.length;
    } else {
      return;
    }
    const target = buttons[next];
    target?.focus();
    target?.click();
  };

  return (
    <>
      <h1 className="step-title" tabIndex={-1}>
        Choose Your Theme
      </h1>
      <p className="step-desc">
        Pick how the Fluence settings window looks. You can change this later from the sidebar.
      </p>
      <div className="step-content">
        <div
          ref={groupRef}
          role="radiogroup"
          aria-label="Color theme"
          className="theme-selector"
          onKeyDown={onGroupKeyDown}
        >
          {THEMES.map((t) => (
            <button
              key={t.value}
              type="button"
              role="radio"
              aria-checked={data.themeMode === t.value}
              aria-label={`${t.label}: ${t.desc}`}
              className={`theme-option${
                data.themeMode === t.value ? ' selected' : ''
              }`}
              onClick={() => onPatch({ themeMode: t.value })}
            >
              <span className={`theme-mock theme-mock-${t.value}`} aria-hidden="true">
                <span className="theme-mock-side" />
                <span className="theme-mock-main">
                  <i />
                  <i />
                  <i />
                </span>
              </span>
              <span className="theme-label">{t.label}</span>
              <span className="theme-desc">{t.desc}</span>
            </button>
          ))}
        </div>
      </div>
    </>
  );
}
