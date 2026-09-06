import { useCallback, useEffect, useRef, useState } from 'react';
import { Button } from '@/components/ui/button';
import { toast } from '@/components/fluence/Toasts';
import {
  getCachedSettings,
  loadSettings,
  saveSettingsNow,
  setSettingField,
} from '@/ipc/settings';
import {
  buildHotkeyString,
  DEFAULT_AGENT_HOTKEY,
  DEFAULT_HOTKEY,
  initGeneralApply,
  listAudioDevices,
  MODIFIER_KEYS,
  persistGeneral,
  setHotkeyRecording,
} from '@/ipc/general';

type HotkeyKey = 'hotkey' | 'agent_hotkey';

function str(value: unknown, fallback: string): string {
  return typeof value === 'string' && value ? value : fallback;
}

// Faithful port of the vanilla General surface (#page-general +
// setupHotkeyRecorders/wireHotkeyRecorder, GENERAL_BINDINGS auto-apply,
// populateAudioDevices, setupSaveButtons general branch). Same DOM
// ids/classes, same copy, same toasts, same debounced persistence with the
// vanilla flush features ('hotkeys' re-registers, 'autostart' applies).
export function GeneralPage() {
  const [hotkey, setHotkey] = useState(DEFAULT_HOTKEY);
  const [agentHotkey, setAgentHotkey] = useState(DEFAULT_AGENT_HOTKEY);
  const [recordingMode, setRecordingMode] = useState('push_to_toggle');
  const [agentRecordingMode, setAgentRecordingMode] = useState('push_to_toggle');
  const [overlayStyle, setOverlayStyle] = useState('full');
  const [overlayPosition, setOverlayPosition] = useState('bottom_right');
  const [language, setLanguage] = useState('en');
  const [aiPolish, setAiPolish] = useState('none');
  const [audioDevice, setAudioDevice] = useState('');
  const [devices, setDevices] = useState<string[]>([]);
  const [autostart, setAutostart] = useState(false);
  const [duck, setDuck] = useState(false);
  const [autoGrab, setAutoGrab] = useState(false);
  const [sound, setSound] = useState(false);

  const [recording, setRecording] = useState<HotkeyKey | null>(null);
  const [pendingText, setPendingText] = useState('Press your shortcut…');
  const recordingRef = useRef<HotkeyKey | null>(null);
  recordingRef.current = recording;
  const pendingKeys = useRef<Set<string>>(new Set());
  const pendingHotkey = useRef('');
  const displays = useRef<Record<HotkeyKey, HTMLDivElement | null>>({
    hotkey: null,
    agent_hotkey: null,
  });
  const clearBtns = useRef<Record<HotkeyKey, HTMLButtonElement | null>>({
    hotkey: null,
    agent_hotkey: null,
  });

  const setHotkeyValue = useCallback((key: HotkeyKey, value: string) => {
    if (key === 'hotkey') setHotkey(value);
    else setAgentHotkey(value);
  }, []);

  const stopRecordingUi = useCallback(() => {
    pendingKeys.current = new Set();
    pendingHotkey.current = '';
    setRecording(null);
  }, []);

  const cancelRecording = useCallback(() => {
    const key = recordingRef.current;
    if (!key) return;
    // Vanilla restores the stored shortcut, falling back to the
    // transcription default for both recorders.
    setHotkeyValue(key, str(getCachedSettings()?.[key], DEFAULT_HOTKEY));
    stopRecordingUi();
  }, [setHotkeyValue, stopRecordingUi]);

  const applyRecording = useCallback(() => {
    const key = recordingRef.current;
    const value = pendingHotkey.current;
    if (!key || !value) return;
    setHotkeyValue(key, value);
    if (getCachedSettings()) {
      setSettingField(key, value);
      persistGeneral('hotkeys');
    }
    stopRecordingUi();
  }, [setHotkeyValue, stopRecordingUi]);

  // Boot: mirror vanilla populateUI general branch. Controls keep their
  // static-markup defaults when settings fail to load.
  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        await loadSettings();
      } catch {
        /* controls keep vanilla defaults until settings arrive */
      }
      if (cancelled) return;
      const s = getCachedSettings();
      if (s) {
        setHotkey(str(s.hotkey, DEFAULT_HOTKEY));
        setRecordingMode(str(s.recording_mode, 'push_to_toggle'));
        setAgentHotkey(str(s.agent_hotkey, DEFAULT_AGENT_HOTKEY));
        setAgentRecordingMode(str(s.agent_recording_mode, 'push_to_toggle'));
        setOverlayPosition(str(s.overlay_position, 'bottom_right'));
        setOverlayStyle(str(s.overlay_style, 'full'));
        setLanguage(str(s.language, 'en'));
        setAutostart(s.auto_start === true);
        setDuck(s.duck_enabled === true);
        setAiPolish(str(s.ai_polish_style, 'none'));
        setAutoGrab(s.auto_grab_highlight !== false);
        setSound((s.sound_on_complete as boolean) ?? true);
        const storedDevice =
          typeof s.audio_device_id === 'string' ? s.audio_device_id : '';
        setAudioDevice(storedDevice);
        initGeneralApply();
      }
      try {
        const list = await listAudioDevices();
        if (cancelled) return;
        setDevices(list);
      } catch {
        // Audio devices unavailable - silently fail
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  // Document-level capture while a recorder is active: mirrors vanilla's
  // global keydown/keyup/click-outside handlers one-to-one.
  useEffect(() => {
    if (!recording) return;
    setHotkeyRecording(true);
    const onKeyDown = (e: KeyboardEvent) => {
      if (!recordingRef.current) return;
      e.preventDefault();
      if (e.key === 'Escape') {
        e.stopImmediatePropagation();
        cancelRecording();
        return;
      }
      pendingKeys.current.add(e.key);
      const parts = buildHotkeyString(e);
      pendingHotkey.current = parts;
      setPendingText(parts || 'Press keys…');
    };
    const onKeyUp = (e: KeyboardEvent) => {
      if (!recordingRef.current) return;
      if (MODIFIER_KEYS.has(e.key)) return;
      if (pendingHotkey.current && pendingKeys.current.size > 0) {
        applyRecording();
      }
    };
    const onClick = (e: MouseEvent) => {
      const key = recordingRef.current;
      if (!key) return;
      const target = e.target instanceof Element ? e.target : null;
      if (!target) return;
      const inside =
        displays.current[key]?.contains(target) ||
        clearBtns.current[key]?.contains(target);
      if (!inside) cancelRecording();
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
  }, [recording, applyRecording, cancelRecording]);

  // Ctrl+S quiet save (vanilla saveGeneral alias: flush, no toast).
  useEffect(() => {
    const onSave = () => {
      saveSettingsNow().catch((err) =>
        toast('Failed to save settings: ' + String(err), 'error'),
      );
    };
    window.addEventListener('fluence:save-page', onSave);
    return () => window.removeEventListener('fluence:save-page', onSave);
  }, []);

  const startRecording = (key: HotkeyKey) => {
    if (recordingRef.current === key) {
      cancelRecording();
      return;
    }
    if (recordingRef.current) cancelRecording();
    pendingKeys.current = new Set();
    pendingHotkey.current = '';
    setPendingText('Press your shortcut…');
    setRecording(key);
  };

  const onResetHotkey = (key: HotkeyKey, fallback: string) => {
    setHotkeyValue(key, fallback);
    if (getCachedSettings()) {
      setSettingField(key, fallback);
      persistGeneral('hotkeys');
    }
  };

  const bindSelect =
    (setter: (v: string) => void, key: string, ...features: string[]) =>
    (value: string) => {
      setter(value);
      if (getCachedSettings()) {
        setSettingField(key, value);
        persistGeneral(...features);
      }
    };

  const bindCheck =
    (setter: (v: boolean) => void, key: string, ...features: string[]) =>
    (value: boolean) => {
      setter(value);
      if (getCachedSettings()) {
        setSettingField(key, value);
        persistGeneral(...features);
      }
    };

  const onSaveAll = async () => {
    try {
      await saveSettingsNow();
    } catch (err) {
      toast('Failed to save settings: ' + String(err), 'error');
    }
    toast('Settings saved ✓', 'success');
  };

  return (
    <section className="page active" id="page-general">
      <div className="page-header">
        <h1 className="page-title" tabIndex={-1}>General</h1>
        <p className="page-subtitle">Hotkey, recording mode, overlay, and system preferences</p>
      </div>

      <div className="settings-section">
        <div className="settings-section-header"><h2>Global Shortcut</h2></div>
        <div className="setting-row">
          <div className="setting-info">
            <div className="setting-label" id="hotkey-label">Recording Hotkey</div>
            <div className="setting-desc">Press this key combination to start/stop voice recording from any app</div>
          </div>
          <div className="setting-control">
            <div className="hotkey-recorder">
              <div
                ref={(el) => {
                  displays.current.hotkey = el;
                }}
                className={`hotkey-display${recording === 'hotkey' ? ' recording' : ''}`}
                id="hotkey-display"
                tabIndex={0}
                role="button"
                aria-labelledby="hotkey-label hotkey-display-text"
                onClick={() => startRecording('hotkey')}
                onBlur={() => {
                  if (recordingRef.current === 'hotkey') cancelRecording();
                }}
                onKeyDown={(e) => {
                  if (recordingRef.current) return;
                  if (e.key === 'Enter' || e.key === ' ') {
                    e.preventDefault();
                    e.stopPropagation();
                    startRecording('hotkey');
                  }
                }}
              >
                <span id="hotkey-display-text">
                  {recording === 'hotkey' ? pendingText : hotkey}
                </span>
              </div>
              <button
                ref={(el) => {
                  clearBtns.current.hotkey = el;
                }}
                type="button"
                className="btn-ghost btn-small"
                id="hotkey-clear-btn"
                onClick={() => onResetHotkey('hotkey', DEFAULT_HOTKEY)}
              >
                Reset
              </button>
            </div>
          </div>
        </div>
        <div className="setting-row">
          <div className="setting-info">
            <div className="setting-label">Transcription Mode Behavior</div>
            <div className="setting-desc">How the hotkey controls recording</div>
          </div>
          <div className="setting-control">
            <select
              id="recording-mode-select"
              className="select-md"
              aria-label="Transcription mode behavior"
              value={recordingMode}
              onChange={(e) =>
                bindSelect(setRecordingMode, 'recording_mode', 'hotkeys')(e.target.value)
              }
            >
              <option value="push_to_toggle">Push-to-Toggle</option>
              <option value="hold_to_record">Hold-to-Record</option>
            </select>
          </div>
        </div>
        <div className="setting-row">
          <div className="setting-info">
            <div className="setting-label" id="agent-hotkey-label">Agent Mode Hotkey</div>
            <div className="setting-desc">Dedicated hotkey to activate AI Agent Mode from any app</div>
          </div>
          <div className="setting-control">
            <div className="hotkey-recorder">
              <div
                ref={(el) => {
                  displays.current.agent_hotkey = el;
                }}
                className={`hotkey-display${recording === 'agent_hotkey' ? ' recording' : ''}`}
                id="agent-hotkey-display"
                tabIndex={0}
                role="button"
                aria-labelledby="agent-hotkey-label agent-hotkey-display-text"
                onClick={() => startRecording('agent_hotkey')}
                onBlur={() => {
                  if (recordingRef.current === 'agent_hotkey') cancelRecording();
                }}
                onKeyDown={(e) => {
                  if (recordingRef.current) return;
                  if (e.key === 'Enter' || e.key === ' ') {
                    e.preventDefault();
                    e.stopPropagation();
                    startRecording('agent_hotkey');
                  }
                }}
              >
                <span id="agent-hotkey-display-text">
                  {recording === 'agent_hotkey' ? pendingText : agentHotkey}
                </span>
              </div>
              <button
                ref={(el) => {
                  clearBtns.current.agent_hotkey = el;
                }}
                type="button"
                className="btn-ghost btn-small"
                id="agent-hotkey-clear-btn"
                onClick={() => onResetHotkey('agent_hotkey', DEFAULT_AGENT_HOTKEY)}
              >
                Reset
              </button>
            </div>
          </div>
        </div>
        <div className="setting-row">
          <div className="setting-info">
            <div className="setting-label">Agent Mode Behavior</div>
            <div className="setting-desc">How the agent hotkey controls recording</div>
          </div>
          <div className="setting-control">
            <select
              id="agent-recording-mode-select"
              className="select-md"
              aria-label="Agent mode behavior"
              value={agentRecordingMode}
              onChange={(e) =>
                bindSelect(setAgentRecordingMode, 'agent_recording_mode', 'hotkeys')(e.target.value)
              }
            >
              <option value="push_to_toggle">Push-to-Toggle</option>
              <option value="hold_to_record">Hold-to-Record</option>
            </select>
          </div>
        </div>
      </div>

      <div className="settings-section">
        <div className="settings-section-header"><h2>Recording Overlay</h2></div>
        <div className="setting-row">
          <div className="setting-info">
            <div className="setting-label">Overlay Style</div>
            <div className="setting-desc">Choose between the full telemetry card, compact pill, or minimal circular bubble</div>
          </div>
          <div className="setting-control">
            <select
              id="overlay-style-select"
              className="select-md"
              aria-label="Overlay style"
              value={overlayStyle}
              onChange={(e) => bindSelect(setOverlayStyle, 'overlay_style')(e.target.value)}
            >
              <option value="full">Full Status Island</option>
              <option value="compact">Compact Pill</option>
              <option value="bubble">Minimal Bubble</option>
            </select>
          </div>
        </div>
        <div className="setting-row">
          <div className="setting-info">
            <div className="setting-label">Overlay Position</div>
            <div className="setting-desc">Where the floating overlay appears on screen during recording</div>
          </div>
          <div className="setting-control">
            <select
              id="overlay-position-select"
              className="select-md"
              aria-label="Overlay position"
              value={overlayPosition}
              onChange={(e) => bindSelect(setOverlayPosition, 'overlay_position')(e.target.value)}
            >
              <option value="bottom_right">Bottom Right</option>
              <option value="bottom_left">Bottom Left</option>
              <option value="center">Center Bottom</option>
            </select>
          </div>
        </div>
      </div>

      <div className="settings-section">
        <div className="settings-section-header"><h2>Audio Input</h2></div>
        <div className="setting-row">
          <div className="setting-info">
            <div className="setting-label">Microphone</div>
            <div className="setting-desc">Select the audio input device for voice recording</div>
          </div>
          <div className="setting-control">
            <select
              id="audio-device-select"
              className="select-lg"
              aria-label="Microphone input device"
              value={audioDevice}
              onChange={(e) => bindSelect(setAudioDevice, 'audio_device_id')(e.target.value)}
            >
              <option value="">System Default</option>
              {devices.map((name) => (
                <option key={name} value={name}>{name}</option>
              ))}
              {audioDevice && !devices.includes(audioDevice) && (
                <option value={audioDevice}>{audioDevice}</option>
              )}
            </select>
          </div>
        </div>
        <div className="setting-row">
          <div className="setting-info">
            <div className="setting-label">Transcription Language</div>
            <div className="setting-desc">Hint to the STT model about the spoken language</div>
          </div>
          <div className="setting-control">
            <select
              id="language-select"
              className="select-md"
              aria-label="Transcription language"
              value={language}
              onChange={(e) => bindSelect(setLanguage, 'language')(e.target.value)}
            >
              <option value="en">English</option>
              <option value="auto">Auto-detect</option>
              <option value="es">Spanish</option>
              <option value="fr">French</option>
              <option value="de">German</option>
              <option value="zh">Chinese</option>
              <option value="ja">Japanese</option>
              <option value="hi">Hindi</option>
              <option value="ar">Arabic</option>
              <option value="pt">Portuguese</option>
              <option value="it">Italian</option>
              <option value="nl">Dutch</option>
              <option value="ko">Korean</option>
              <option value="ru">Russian</option>
              <option value="mr">Marathi</option>
              <option value="pa">Punjabi</option>
              <option value="hu">Hungarian</option>
            </select>
          </div>
        </div>
        <div className="setting-row">
          <div className="setting-info">
            <div className="setting-label">AI Polish Style</div>
            <div className="setting-desc">Automatically rewrite or clean up text before pasting</div>
          </div>
          <div className="setting-control">
            <select
              id="ai-polish-select"
              className="select-md"
              aria-label="AI polish style"
              value={aiPolish}
              onChange={(e) => bindSelect(setAiPolish, 'ai_polish_style')(e.target.value)}
            >
              <option value="none">None (Raw)</option>
              <option value="clean">Clean Fillers &amp; Grammar</option>
              <option value="professional">Professional Tone</option>
              <option value="bullet_points">Bulleted List</option>
              <option value="translate_en">Translate to English</option>
            </select>
          </div>
        </div>
        <div className="setting-row">
          <div className="setting-info">
            <div className="setting-label">Play Completion Sound</div>
            <div className="setting-desc">Play a short chime when a transcription finishes</div>
          </div>
          <div className="setting-control">
            <label className="toggle-switch" id="sound-on-complete-toggle">
              <input
                type="checkbox"
                id="sound-on-complete-cb"
                aria-label="Play a sound when transcription completes"
                checked={sound}
                onChange={(e) => bindCheck(setSound, 'sound_on_complete')(e.target.checked)}
              />
              <div className="toggle-track"><div className="toggle-thumb" /></div>
            </label>
          </div>
        </div>
      </div>

      <div className="settings-section">
        <div className="settings-section-header"><h2>System</h2></div>
        <div className="setting-row">
          <div className="setting-info">
            <div className="setting-label">Launch at Windows Startup</div>
            <div className="setting-desc">Automatically start Fluence when you log in to Windows</div>
          </div>
          <div className="setting-control">
            <label className="toggle-switch" id="autostart-toggle">
              <input
                type="checkbox"
                id="autostart-cb"
                aria-label="Launch at Windows startup"
                checked={autostart}
                onChange={(e) => bindCheck(setAutostart, 'auto_start', 'autostart')(e.target.checked)}
              />
              <div className="toggle-track"><div className="toggle-thumb" /></div>
            </label>
          </div>
        </div>
        <div className="setting-row">
          <div className="setting-info">
            <div className="setting-label">Mute Background Apps</div>
            <div className="setting-desc">Silence music, videos, and calls while you dictate</div>
          </div>
          <div className="setting-control">
            <label className="toggle-switch" id="duck-toggle">
              <input
                type="checkbox"
                id="duck-cb"
                aria-label="Mute background apps while dictating"
                checked={duck}
                onChange={(e) => bindCheck(setDuck, 'duck_enabled')(e.target.checked)}
              />
              <div className="toggle-track"><div className="toggle-thumb" /></div>
            </label>
          </div>
        </div>
        <div className="setting-row">
          <div className="setting-info">
            <div className="setting-label">Grab Highlighted Text</div>
            <div className="setting-desc">Automatically read highlighted text when entering Agent Mode</div>
          </div>
          <div className="setting-control">
            <label className="toggle-switch" id="auto-grab-toggle">
              <input
                type="checkbox"
                id="auto-grab-cb"
                aria-label="Grab highlighted text when entering Agent Mode"
                checked={autoGrab}
                onChange={(e) => bindCheck(setAutoGrab, 'auto_grab_highlight')(e.target.checked)}
              />
              <div className="toggle-track"><div className="toggle-thumb" /></div>
            </label>
          </div>
        </div>
      </div>

      <div style={{ display: 'flex', justifyContent: 'flex-end', paddingTop: 'var(--spacing-md)' }}>
        <Button variant="primary" id="save-general-btn" style={{ minWidth: 120 }} onClick={() => void onSaveAll()}>Save Changes</Button>
      </div>
    </section>
  );
}
