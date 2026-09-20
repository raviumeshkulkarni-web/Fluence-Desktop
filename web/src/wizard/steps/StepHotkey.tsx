import { useRef } from 'react';
import type { KeyboardEvent } from 'react';
import type { WizardData } from '../Wizard';
import { HotkeyRecorder } from '../HotkeyRecorder';

interface StepHotkeyProps {
  data: WizardData;
  onPatch: (patch: Partial<WizardData>) => void;
  active: boolean;
  onRecordingChange: (recording: boolean) => void;
}

const MODES = [
  {
    value: 'push_to_toggle',
    title: 'Push-to-Toggle',
    desc: 'Press once to start, press again to stop',
  },
  {
    value: 'hold_to_record',
    title: 'Hold-to-Record',
    desc: 'Hold key to record, release to transcribe',
  },
] as const;

// Step 5: hotkey recorder plus recording-mode selector. The modes are real
// buttons in a radiogroup (roving arrows, one tab stop) — the previous
// RadioGroupItem-as-card swallowed its children, which is why the options
// rendered as empty boxes.
export function StepHotkey({
  data,
  onPatch,
  active,
  onRecordingChange,
}: StepHotkeyProps) {
  const groupRef = useRef<HTMLDivElement>(null);

  const onGroupKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    const buttons = Array.from(
      groupRef.current?.querySelectorAll<HTMLButtonElement>('.seg-option') ?? [],
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
        Set Your Hotkey
      </h1>
      <p className="step-desc">
        Choose a global keyboard shortcut to start recording. It will work in any application.
      </p>
      <div className="step-content">
        <HotkeyRecorder
          value={data.hotkey}
          onChange={(hotkey) => onPatch({ hotkey })}
          active={active}
          onRecordingChange={onRecordingChange}
        />
        <div className="form-row">
          <span id="wiz-mode-label" className="field-label">
            Recording Mode
          </span>
          <div
            ref={groupRef}
            role="radiogroup"
            aria-labelledby="wiz-mode-label"
            className="seg-selector"
            onKeyDown={onGroupKeyDown}
          >
            {MODES.map((m) => (
              <button
                key={m.value}
                type="button"
                role="radio"
                aria-checked={data.recordingMode === m.value}
                data-mode={m.value}
                className={`seg-option${
                  data.recordingMode === m.value ? ' selected' : ''
                }`}
                onClick={() => onPatch({ recordingMode: m.value })}
              >
                <span className="seg-title">{m.title}</span>
                <span className="seg-desc">{m.desc}</span>
              </button>
            ))}
          </div>
        </div>
        <p style={{ fontSize: 'var(--text-label-sm)', color: 'var(--color-on-surface-variant)', margin: 0 }}>
          <strong>Tip:</strong> Long press (&gt;800ms) activates Agent Mode for AI-powered editing commands
        </p>
      </div>
    </>
  );
}
