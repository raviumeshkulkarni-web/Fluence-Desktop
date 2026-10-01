import { RadioGroup, RadioGroupItem } from '@/components/ui/radio-group';
import type { WizardData } from '../Wizard';

interface StepPositionProps {
  data: WizardData;
  onPatch: (patch: Partial<WizardData>) => void;
}

const POSITIONS = [
  { value: 'bottom_left', cls: 'pos-left', label: 'Bottom Left' },
  { value: 'center', cls: 'pos-center', label: 'Center' },
  { value: 'bottom_right', cls: 'pos-right', label: 'Bottom Right' },
] as const;

// Step 6: overlay position. Real buttons in a radiogroup — the previous
// RadioGroupItem-as-card swallowed its children, so the previews and labels
// never rendered and the step read as three ambiguous boxes. Each option now
// shows a screen mock with the indicator dot plus a visible label.
export function StepPosition({ data, onPatch }: StepPositionProps) {
  return (
    <>
      <h1 className="step-title" tabIndex={-1}>
        Overlay Position
      </h1>
      <p className="step-desc">
        Choose where the floating recording indicator appears on your screen during voice capture.
      </p>
      <div className="step-content">
        <RadioGroup
          className="position-selector"
          value={data.overlayPosition}
          onValueChange={(overlayPosition) => onPatch({ overlayPosition })}
          aria-label="Overlay position"
        >
          {POSITIONS.map((p) => (
            <RadioGroupItem key={p.value} value={p.value} asChild>
              <button
                type="button"
                aria-label={p.label}
                data-pos={p.value}
                className={`choice-surface position-option ${p.cls}`}
              >
                <span className="position-preview" aria-hidden="true">
                  <span className="position-dot" />
                </span>
                <span className="position-label">{p.label}</span>
              </button>
            </RadioGroupItem>
          ))}
        </RadioGroup>
      </div>
    </>
  );
}
