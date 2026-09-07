import { RadioGroup, RadioGroupItem } from '@/components/ui/radio-group';
import type { WizardData } from '../Wizard';
import { HotkeyRecorder } from '../HotkeyRecorder';

interface StepHotkeyProps {
  data: WizardData;
  onPatch: (patch: Partial<WizardData>) => void;
  active: boolean;
  onRecordingChange: (recording: boolean) => void;
}

// Step 3: hotkey recorder plus recording-mode selector. The mode options
// render as radio items in the vanilla .mode-option chrome (selected class
// follows the checked value); keyboard moves with arrows, one tab stop.
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
          <label id="wiz-mode-label">Recording Mode</label>
          <RadioGroup
            aria-labelledby="wiz-mode-label"
            value={data.recordingMode}
            onValueChange={(v) => onPatch({ recordingMode: v })}
            className="mode-selector"
          >
            <RadioGroupItem
              value="push_to_toggle"
              data-mode="push_to_toggle"
              className={`mode-option${data.recordingMode === 'push_to_toggle' ? ' selected' : ''}`}
            >
              <div className="mode-title">Push-to-Toggle</div>
              <div className="mode-desc">Press once to start, press again to stop</div>
            </RadioGroupItem>
            <RadioGroupItem
              value="hold_to_record"
              data-mode="hold_to_record"
              className={`mode-option${data.recordingMode === 'hold_to_record' ? ' selected' : ''}`}
            >
              <div className="mode-title">Hold-to-Record</div>
              <div className="mode-desc">Hold key to record, release to transcribe</div>
            </RadioGroupItem>
          </RadioGroup>
        </div>
        <p style={{ fontSize: 'var(--text-label-sm)', color: 'var(--color-on-surface-variant)', marginTop: 4 }}>
          <strong>Tip:</strong> Long press (&gt;800ms) activates Agent Mode for AI-powered editing commands
        </p>
      </div>
    </>
  );
}
