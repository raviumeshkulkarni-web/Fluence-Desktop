import { useEffect, useRef, useState } from 'react';
import { Button } from '@/components/ui/button';
import {
  buildHotkeyString,
  MODIFIER_KEYS,
  setHotkeyRecording,
} from '@/ipc/general';
import {
  displayHotkey,
  isAllowedWizardHotkey as isAllowedHotkey,
} from '@/wizard/hotkey';

export interface HotkeyRecorderFieldProps {
  value: string;
  isRecording: boolean;
  labelId: string;
  displayId: string;
  displayTextId: string;
  clearBtnId: string;
  onStartRecording: () => void;
  onStopRecording: () => void;
  onCommit: (hotkey: string) => void;
  onReset: () => void;
}

export function HotkeyRecorderField({
  value,
  isRecording,
  labelId,
  displayId,
  displayTextId,
  clearBtnId,
  onStartRecording,
  onStopRecording,
  onCommit,
  onReset,
}: HotkeyRecorderFieldProps) {
  const [pendingText, setPendingText] = useState('Press your shortcut…');
  const [hint, setHint] = useState<string | null>(null);
  const pendingKeys = useRef<Set<string>>(new Set());
  const pendingHotkey = useRef('');
  const displayRef = useRef<HTMLButtonElement | null>(null);
  const clearBtnRef = useRef<HTMLButtonElement | null>(null);
  const onCommitRef = useRef(onCommit);
  onCommitRef.current = onCommit;
  const onStopRef = useRef(onStopRecording);
  onStopRef.current = onStopRecording;

  useEffect(() => {
    if (!isRecording) {
      pendingKeys.current = new Set();
      pendingHotkey.current = '';
      setPendingText('Press your shortcut…');
      setHint(null);
      return;
    }

    setHotkeyRecording(true);
    setPendingText('Press your shortcut…');
    setHint(null);

    const onKeyDown = (e: KeyboardEvent) => {
      e.preventDefault();
      if (e.key === 'Escape') {
        e.stopImmediatePropagation();
        onStopRef.current();
        return;
      }
      pendingKeys.current.add(e.key);
      const parts = buildHotkeyString(e);
      pendingHotkey.current = parts;
      setPendingText(parts || 'Press keys…');
      setHint(null);
    };

    const onKeyUp = (e: KeyboardEvent) => {
      if (MODIFIER_KEYS.has(e.key)) return;
      const candidate = pendingHotkey.current;
      if (!candidate || pendingKeys.current.size === 0) return;
      if (!isAllowedHotkey(candidate)) {
        setHint('Add Ctrl, Alt, Shift, or Win. Function keys (F1 to F24) can be used alone.');
        pendingKeys.current = new Set();
        pendingHotkey.current = '';
        setPendingText('Press your shortcut…');
        return;
      }
      onCommitRef.current(candidate);
      onStopRef.current();
    };

    const onClick = (e: MouseEvent) => {
      const target = e.target instanceof Element ? e.target : null;
      if (!target) return;
      const inside =
        displayRef.current?.contains(target) ||
        clearBtnRef.current?.contains(target);
      if (!inside) onStopRef.current();
    };

    document.addEventListener('keydown', onKeyDown);
    document.addEventListener('keyup', onKeyUp);
    document.addEventListener('click', onClick);
    return () => {
      document.removeEventListener('keydown', onKeyDown);
      document.removeEventListener('keyup', onKeyUp);
      document.removeEventListener('click', onClick);
      setHotkeyRecording(false);
    };
  }, [isRecording]);

  return (
    <div className="hotkey-recorder-wrap">
      <div className="hotkey-recorder">
        <button
          ref={displayRef}
          type="button"
          className={`hotkey-display ui-focus-ring${isRecording ? ' recording' : ''}`}
          id={displayId}
          aria-pressed={isRecording}
          aria-labelledby={`${labelId} ${displayTextId}`}
          onClick={() => {
            if (isRecording) onStopRecording();
            else onStartRecording();
          }}
          onBlur={() => {
            if (isRecording) onStopRecording();
          }}
        >
          <span id={displayTextId}>
            {displayHotkey(isRecording ? pendingText : value)}
          </span>
        </button>
        <Button
          id={clearBtnId}
          ref={clearBtnRef}
          type="button"
          variant="ghost"
          size="sm"
          onClick={() => {
            if (isRecording) onStopRecording();
            onReset();
          }}
        >
          Reset
        </Button>
      </div>
      {isRecording ? (
        <div className="hotkey-inline-hint" role="status">
          {hint ?? 'Press a shortcut with Ctrl, Alt, Shift, or Win (Esc to cancel).'}
        </div>
      ) : null}
    </div>
  );
}
