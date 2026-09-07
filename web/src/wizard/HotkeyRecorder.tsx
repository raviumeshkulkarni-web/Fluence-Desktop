import { useEffect, useRef, useState } from 'react';
import { WIZARD_MODIFIER_TOKENS, buildWizardHotkey } from './hotkey';

interface HotkeyRecorderProps {
  value: string;
  onChange: (hotkey: string) => void;
  active: boolean;
  onRecordingChange: (recording: boolean) => void;
}

// Wizard-local hotkey recorder. Mirrors vanilla wizard.js setupStep3 exactly:
// click/Enter/Space arms it, live parts render while held, keyup commits
// (modifiers-only keeps recording), Esc/blur/outside-click cancels.
export function HotkeyRecorder({ value, onChange, active, onRecordingChange }: HotkeyRecorderProps) {
  const [recording, setRecording] = useState(false);
  const [pending, setPending] = useState('');
  const recordingRef = useRef(false);
  recordingRef.current = recording;
  const pendingRef = useRef('');
  const displayRef = useRef<HTMLDivElement | null>(null);
  const onChangeRef = useRef(onChange);
  onChangeRef.current = onChange;
  const notifyRef = useRef(onRecordingChange);
  notifyRef.current = onRecordingChange;

  const stop = (commit: boolean) => {
    if (commit && pendingRef.current) onChangeRef.current(pendingRef.current);
    pendingRef.current = '';
    setPending('');
    setRecording(false);
    notifyRef.current(false);
  };

  const start = () => {
    if (recordingRef.current) return;
    pendingRef.current = '';
    setPending('');
    setRecording(true);
    notifyRef.current(true);
  };

  useEffect(() => {
    if (!recording || !active) return;
    const onKeyDown = (e: KeyboardEvent) => {
      if (!recordingRef.current) return;
      e.preventDefault();
      if (e.key === 'Escape') {
        stop(false);
        return;
      }
      const parts = buildWizardHotkey(e);
      if (parts) {
        pendingRef.current = parts;
        setPending(parts);
      }
    };
    const onKeyUp = () => {
      if (!recordingRef.current) return;
      const current = pendingRef.current;
      if (current && !current.split('+').every((t) => WIZARD_MODIFIER_TOKENS.has(t))) {
        stop(true);
      }
    };
    const onClick = (e: MouseEvent) => {
      if (!recordingRef.current) return;
      const target = e.target instanceof Node ? e.target : null;
      if (target && displayRef.current?.contains(target)) return;
      stop(false);
    };
    document.addEventListener('keydown', onKeyDown);
    document.addEventListener('keyup', onKeyUp);
    document.addEventListener('click', onClick);
    return () => {
      document.removeEventListener('keydown', onKeyDown);
      document.removeEventListener('keyup', onKeyUp);
      document.removeEventListener('click', onClick);
    };
  }, [recording, active]);

  return (
    <div className="form-row" style={{ alignItems: 'center' }}>
      <label id="wiz-hotkey-label">Recording Shortcut</label>
      <div
        ref={displayRef}
        id="wiz-hotkey-display"
        role="button"
        tabIndex={0}
        aria-labelledby="wiz-hotkey-label wiz-hotkey-text"
        className={`hotkey-display${recording ? ' recording' : ''}`}
        onClick={start}
        onBlur={() => {
          if (recordingRef.current) stop(false);
        }}
        onKeyDown={(e) => {
          if (recordingRef.current) return;
          if (e.key === 'Enter' || e.key === ' ') {
            e.preventDefault();
            e.stopPropagation();
            start();
          }
        }}
      >
        <span id="wiz-hotkey-text">{recording ? pending || 'Press your shortcut…' : value}</span>
      </div>
    </div>
  );
}
