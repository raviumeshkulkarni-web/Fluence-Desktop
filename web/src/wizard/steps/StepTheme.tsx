import { RadioGroup, RadioGroupItem } from '@/components/ui/radio-group';
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
  return (
    <>
      <h1 className="step-title" tabIndex={-1}>
        Choose Your Theme
      </h1>
      <p className="step-desc">
        Pick how the Fluence settings window looks. You can change this later from the sidebar.
      </p>
      <div className="step-content">
        <RadioGroup
          className="theme-selector"
          value={data.themeMode}
          onValueChange={(themeMode) => onPatch({ themeMode: themeMode as ThemeMode })}
          aria-label="Color theme"
        >
          {THEMES.map((t) => (
            <RadioGroupItem key={t.value} value={t.value} asChild>
              <button
                type="button"
                aria-label={`${t.label}: ${t.desc}`}
                className="choice-surface theme-option"
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
            </RadioGroupItem>
          ))}
        </RadioGroup>
      </div>
    </>
  );
}
