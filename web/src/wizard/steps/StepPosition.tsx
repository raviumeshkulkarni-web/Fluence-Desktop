import { useRef } from 'react';
import type { KeyboardEvent } from 'react';
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
  const groupRef = useRef<HTMLDivElement>(null);

  const onGroupKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    const buttons = Array.from(
      groupRef.current?.querySelectorAll<HTMLButtonElement>('.position-option') ?? [],
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
        Overlay Position
      </h1>
      <p className="step-desc">
        Choose where the floating recording indicator appears on your screen during voice capture.
      </p>
      <div className="step-content">
        <div
          ref={groupRef}
          role="radiogroup"
          aria-label="Overlay position"
          className="position-selector"
          onKeyDown={onGroupKeyDown}
        >
          {POSITIONS.map((p) => (
            <button
              key={p.value}
              type="button"
              role="radio"
              aria-checked={data.overlayPosition === p.value}
              aria-label={p.label}
              data-pos={p.value}
              className={`position-option ${p.cls}${
                data.overlayPosition === p.value ? ' selected' : ''
              }`}
              onClick={() => onPatch({ overlayPosition: p.value })}
            >
              <span className="position-preview" aria-hidden="true">
                <span className="position-dot" />
              </span>
              <span className="position-label">{p.label}</span>
            </button>
          ))}
        </div>
      </div>
    </>
  );
}
