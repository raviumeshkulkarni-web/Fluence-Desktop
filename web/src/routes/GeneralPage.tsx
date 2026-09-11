import { useCallback, useEffect, useRef, useState } from 'react';
import { Button } from '@/components/ui/button';
import { Field } from '@/components/ui/field';
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select';
import { Switch } from '@/components/ui/switch';
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

// Radix Select items reject empty-string values; the device list uses ""
// for "System Default", so a sentinel stands in at the kit boundary and is
// mapped back before persistence. Stored values never change.
const SYSTEM_DEFAULT_DEVICE = '__system_default__';

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
    toast('Settings saved', 'success');
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
                className={`hotkey-display ui-focus-ring${recording === 'hotkey' ? ' recording' : ''}`}
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
              <Button
                ref={(el) => {
                  clearBtns.current.hotkey = el;
                }}
                type="button"
                variant="ghost"
                size="sm"
                id="hotkey-clear-btn"
                onClick={() => onResetHotkey('hotkey', DEFAULT_HOTKEY)}
              >
                Reset
              </Button>
            </div>
          </div>
        </div>
        <div className="setting-row">
          <div className="setting-info">
            <div className="setting-label">Transcription Mode Behavior</div>
            <div className="setting-desc">How the hotkey controls recording</div>
          </div>
          <div className="setting-control">
            <Field>
              <Select
                value={recordingMode}
                onValueChange={bindSelect(setRecordingMode, 'recording_mode', 'hotkeys')}
              >
                <SelectTrigger
                  id="recording-mode-select"
                  className="select-md"
                  aria-label="Transcription mode behavior"
                >
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="push_to_toggle">Push-to-Toggle</SelectItem>
                  <SelectItem value="hold_to_record">Hold-to-Record</SelectItem>
                </SelectContent>
              </Select>
            </Field>
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
                className={`hotkey-display ui-focus-ring${recording === 'agent_hotkey' ? ' recording' : ''}`}
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
              <Button
                ref={(el) => {
                  clearBtns.current.agent_hotkey = el;
                }}
                type="button"
                variant="ghost"
                size="sm"
                id="agent-hotkey-clear-btn"
                onClick={() => onResetHotkey('agent_hotkey', DEFAULT_AGENT_HOTKEY)}
              >
                Reset
              </Button>
            </div>
          </div>
        </div>
        <div className="setting-row">
          <div className="setting-info">
            <div className="setting-label">Agent Mode Behavior</div>
            <div className="setting-desc">How the agent hotkey controls recording</div>
          </div>
          <div className="setting-control">
            <Field>
              <Select
                value={agentRecordingMode}
                onValueChange={bindSelect(setAgentRecordingMode, 'agent_recording_mode', 'hotkeys')}
              >
                <SelectTrigger
                  id="agent-recording-mode-select"
                  className="select-md"
                  aria-label="Agent mode behavior"
                >
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="push_to_toggle">Push-to-Toggle</SelectItem>
                  <SelectItem value="hold_to_record">Hold-to-Record</SelectItem>
                </SelectContent>
              </Select>
            </Field>
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
            <Field>
              <Select
                value={overlayStyle}
                onValueChange={bindSelect(setOverlayStyle, 'overlay_style')}
              >
                <SelectTrigger
                  id="overlay-style-select"
                  className="select-md"
                  aria-label="Overlay style"
                >
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="full">Full Status Island</SelectItem>
                  <SelectItem value="compact">Compact Pill</SelectItem>
                  <SelectItem value="bubble">Minimal Bubble</SelectItem>
                </SelectContent>
              </Select>
            </Field>
          </div>
        </div>
        <div className="setting-row">
          <div className="setting-info">
            <div className="setting-label">Overlay Position</div>
            <div className="setting-desc">Where the floating overlay appears on screen during recording</div>
          </div>
          <div className="setting-control">
            <Field>
              <Select
                value={overlayPosition}
                onValueChange={bindSelect(setOverlayPosition, 'overlay_position')}
              >
                <SelectTrigger
                  id="overlay-position-select"
                  className="select-md"
                  aria-label="Overlay position"
                >
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="bottom_right">Bottom Right</SelectItem>
                  <SelectItem value="bottom_left">Bottom Left</SelectItem>
                  <SelectItem value="center">Center Bottom</SelectItem>
                </SelectContent>
              </Select>
            </Field>
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
            <Field>
              <Select
                value={audioDevice || SYSTEM_DEFAULT_DEVICE}
                onValueChange={(v) =>
                  bindSelect(setAudioDevice, 'audio_device_id')(
                    v === SYSTEM_DEFAULT_DEVICE ? '' : v,
                  )
                }
              >
                <SelectTrigger
                  id="audio-device-select"
                  className="select-lg"
                  aria-label="Microphone input device"
                >
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value={SYSTEM_DEFAULT_DEVICE}>System Default</SelectItem>
                  {devices.map((name) => (
                    <SelectItem key={name} value={name}>{name}</SelectItem>
                  ))}
                  {audioDevice && !devices.includes(audioDevice) && (
                    <SelectItem value={audioDevice}>{audioDevice}</SelectItem>
                  )}
                </SelectContent>
              </Select>
            </Field>
          </div>
        </div>
        <div className="setting-row">
          <div className="setting-info">
            <div className="setting-label">Transcription Language</div>
            <div className="setting-desc">Hint to the STT model about the spoken language</div>
          </div>
          <div className="setting-control">
            <Field>
              <Select
                value={language}
                onValueChange={bindSelect(setLanguage, 'language')}
              >
                <SelectTrigger
                  id="language-select"
                  className="select-md"
                  aria-label="Transcription language"
                >
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="en">English</SelectItem>
                  <SelectItem value="auto">Auto-detect</SelectItem>
                  <SelectItem value="es">Spanish</SelectItem>
                  <SelectItem value="fr">French</SelectItem>
                  <SelectItem value="de">German</SelectItem>
                  <SelectItem value="zh">Chinese</SelectItem>
                  <SelectItem value="ja">Japanese</SelectItem>
                  <SelectItem value="hi">Hindi</SelectItem>
                  <SelectItem value="ar">Arabic</SelectItem>
                  <SelectItem value="pt">Portuguese</SelectItem>
                  <SelectItem value="it">Italian</SelectItem>
                  <SelectItem value="nl">Dutch</SelectItem>
                  <SelectItem value="ko">Korean</SelectItem>
                  <SelectItem value="ru">Russian</SelectItem>
                  <SelectItem value="mr">Marathi</SelectItem>
                  <SelectItem value="pa">Punjabi</SelectItem>
                  <SelectItem value="hu">Hungarian</SelectItem>
                </SelectContent>
              </Select>
            </Field>
          </div>
        </div>
        <div className="setting-row">
          <div className="setting-info">
            <div className="setting-label">AI Polish Style</div>
            <div className="setting-desc">Automatically rewrite or clean up text before pasting</div>
          </div>
          <div className="setting-control">
            <Field>
              <Select
                value={aiPolish}
                onValueChange={bindSelect(setAiPolish, 'ai_polish_style')}
              >
                <SelectTrigger
                  id="ai-polish-select"
                  className="select-md"
                  aria-label="AI polish style"
                >
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="none">None (Raw)</SelectItem>
                  <SelectItem value="clean">Clean Fillers &amp; Grammar</SelectItem>
                  <SelectItem value="professional">Professional Tone</SelectItem>
                  <SelectItem value="bullet_points">Bulleted List</SelectItem>
                  <SelectItem value="translate_en">Translate to English</SelectItem>
                </SelectContent>
              </Select>
            </Field>
          </div>
        </div>
        <div className="setting-row">
          <div className="setting-info">
            <div className="setting-label">Play Completion Sound</div>
            <div className="setting-desc">Play a short chime when a transcription finishes</div>
          </div>
          <div className="setting-control">
            <Field>
              <Switch
                id="sound-on-complete-cb"
                aria-label="Play a sound when transcription completes"
                checked={sound}
                onCheckedChange={bindCheck(setSound, 'sound_on_complete')}
              />
            </Field>
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
            <Field>
              <Switch
                id="autostart-cb"
                aria-label="Launch at Windows startup"
                checked={autostart}
                onCheckedChange={bindCheck(setAutostart, 'auto_start', 'autostart')}
              />
            </Field>
          </div>
        </div>
        <div className="setting-row">
          <div className="setting-info">
            <div className="setting-label">Mute Background Apps</div>
            <div className="setting-desc">Silence music, videos, and calls while you dictate</div>
          </div>
          <div className="setting-control">
            <Field>
              <Switch
                id="duck-cb"
                aria-label="Mute background apps while dictating"
                checked={duck}
                onCheckedChange={bindCheck(setDuck, 'duck_enabled')}
              />
            </Field>
          </div>
        </div>
        <div className="setting-row">
          <div className="setting-info">
            <div className="setting-label">Grab Highlighted Text</div>
            <div className="setting-desc">Automatically read highlighted text when entering Agent Mode</div>
          </div>
          <div className="setting-control">
            <Field>
              <Switch
                id="auto-grab-cb"
                aria-label="Grab highlighted text when entering Agent Mode"
                checked={autoGrab}
                onCheckedChange={bindCheck(setAutoGrab, 'auto_grab_highlight')}
              />
            </Field>
          </div>
        </div>
      </div>

      <div style={{ display: 'flex', justifyContent: 'flex-end', paddingTop: 'var(--spacing-md)' }}>
        <Button variant="primary" id="save-general-btn" style={{ minWidth: 120 }} onClick={() => void onSaveAll()}>Save Changes</Button>
      </div>
    </section>
  );
}
