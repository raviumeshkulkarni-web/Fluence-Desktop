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

// Step 4: overlay position as radio items in the vanilla .position-option
// chrome (preview dot + label, selected class follows the checked value).
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
          aria-label="Overlay position"
          value={data.overlayPosition}
          onValueChange={(v) => onPatch({ overlayPosition: v })}
          className="position-selector"
        >
          {POSITIONS.map((p) => (
            <RadioGroupItem
              key={p.value}
              value={p.value}
              data-pos={p.value}
              className={`position-option ${p.cls}${
                data.overlayPosition === p.value ? ' selected' : ''
              }`}
            >
              <div className="position-preview" aria-hidden="true">
                <div className="position-dot" />
              </div>
              <div className="position-label">{p.label}</div>
            </RadioGroupItem>
          ))}
        </RadioGroup>
      </div>
    </>
  );
}
