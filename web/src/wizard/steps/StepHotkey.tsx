import { RadioGroup, RadioGroupItem } from '@/components/ui/radio-group';
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
          <RadioGroup
            className="seg-selector"
            value={data.recordingMode}
            onValueChange={(recordingMode) => onPatch({ recordingMode })}
            aria-labelledby="wiz-mode-label"
          >
            {MODES.map((m) => (
              <RadioGroupItem key={m.value} value={m.value} asChild>
                <button
                  type="button"
                  data-mode={m.value}
                  className="choice-surface seg-option"
                >
                  <span className="seg-title">{m.title}</span>
                  <span className="seg-desc">{m.desc}</span>
                </button>
              </RadioGroupItem>
            ))}
          </RadioGroup>
        </div>
        <p style={{ fontSize: 'var(--text-label-sm)', color: 'var(--color-on-surface-variant)', margin: 0 }}>
          <strong>Tip:</strong> Long press (&gt;800ms) activates Agent Mode for AI-powered editing commands
        </p>
      </div>
    </>
  );
}
