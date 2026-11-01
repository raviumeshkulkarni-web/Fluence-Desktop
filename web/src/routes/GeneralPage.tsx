import { useCallback, useEffect, useRef, useState, type CSSProperties } from 'react';
import { Button } from '@/components/ui/button';
import { Card, CardContent } from '@/components/ui/card';
import { Field } from '@/components/ui/field';
import {
  Select,
  SelectContent,
  SelectGroup,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select';
import { Switch } from '@/components/ui/switch';
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs';
import { RadioGroup, RadioGroupItem } from '@/components/ui/radio-group';
import { toast } from '@/components/fluence/Toasts';
import { SettingsSectionHeader } from '@/components/fluence/SettingsSection';
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
import { ACCENT_THEMES, type AccentTheme } from '@/lib/theme';
import {
  getLanguagePreference,
  getUiLanguage,
  reloadWithLanguage,
  resolveSystemLanguage,
  SYSTEM_LANGUAGE,
  t,
  UI_LANGUAGES,
} from '@/i18n';

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
export function GeneralPage({
  accentTheme,
  onAccentThemeChange,
}: {
  accentTheme: AccentTheme;
  onAccentThemeChange: (accentTheme: AccentTheme) => void;
}) {
  const [hotkey, setHotkey] = useState(DEFAULT_HOTKEY);
  const [agentHotkey, setAgentHotkey] = useState(DEFAULT_AGENT_HOTKEY);
  const [recordingMode, setRecordingMode] = useState('push_to_toggle');
  const [agentRecordingMode, setAgentRecordingMode] = useState('push_to_toggle');
  const [language, setLanguage] = useState('en');
  const [audioDevice, setAudioDevice] = useState('');
  const [devices, setDevices] = useState<string[]>([]);
  const [autostart, setAutostart] = useState(false);
  const [duck, setDuck] = useState(false);
  const [autoGrab, setAutoGrab] = useState(false);
  const [sound, setSound] = useState(false);
  // Seeded with the backend default (ON) so a slow/failing settings load
  // never displays a misleading OFF for the copy-transcript retention.
  const [copyTranscript, setCopyTranscript] = useState(true);
  const [uiLanguage, setUiLanguage] = useState(getLanguagePreference());

  // One concern visible at a time (Providers pattern): shortcuts for the
  // hotkeys, audio for input/language/feedback, system for OS behavior.
  type GeneralTab = 'shortcuts' | 'audio' | 'language' | 'system' | 'appearance';
  const [tab, setTab] = useState<GeneralTab>('shortcuts');

  const [recording, setRecording] = useState<HotkeyKey | null>(null);
  const [pendingText, setPendingText] = useState(() => t('Press your shortcut…'));
  const recordingRef = useRef<HotkeyKey | null>(null);
  recordingRef.current = recording;
  const pendingKeys = useRef<Set<string>>(new Set());
  const pendingHotkey = useRef('');
  const displays = useRef<Record<HotkeyKey, HTMLButtonElement | null>>({
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
        setLanguage(str(s.language, 'en'));
        setUiLanguage(str(s.ui_language, SYSTEM_LANGUAGE));
        setAutostart(s.auto_start === true);
        setDuck(s.duck_enabled === true);
        setAutoGrab(s.auto_grab_highlight !== false);
        setSound((s.sound_on_complete as boolean) ?? true);
        setCopyTranscript(s.copy_transcript_to_clipboard ?? true);
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
      setPendingText(parts || t('Press keys…'));
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
        toast(t('Failed to save settings: {error}', { error: String(err) }), 'error'),
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
    setPendingText(t('Press your shortcut…'));
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
      toast(t('Failed to save settings: {error}', { error: String(err) }), 'error');
      return;
    }
    toast(t('Settings saved'), 'success');
  };

  const onUiLanguageChange = async (value: string) => {
    let settings = getCachedSettings();
    if (!settings) {
      try {
        settings = await loadSettings();
      } catch {
        return;
      }
    }
    setUiLanguage(value);
    setSettingField('ui_language', value);
    try {
      await saveSettingsNow();
    } catch (err) {
      toast(t('Failed to save settings: {error}', { error: String(err) }), 'error');
      return;
    }
    reloadWithLanguage(value);
  };

  const resolvedSystemLanguage =
    uiLanguage === SYSTEM_LANGUAGE
      ? getUiLanguage(resolveSystemLanguage())?.nativeName ?? 'English'
      : null;

  return (
    <section className="page active" id="page-general">
      <div className="page-header">
        <h1 className="page-title" tabIndex={-1}>{t('General')}</h1>
        <p className="page-subtitle">{t('Hotkey, recording mode, audio, and system preferences')}</p>
      </div>

      <Tabs className="page-tabs" value={tab} onValueChange={(v) => setTab(v as GeneralTab)}>
        <TabsList aria-label={t('General settings area')}>
          <TabsTrigger value="shortcuts">{t('Shortcuts')}</TabsTrigger>
          <TabsTrigger value="audio">{t('Audio')}</TabsTrigger>
          <TabsTrigger value="language">{t('Language')}</TabsTrigger>
          <TabsTrigger value="system">{t('System')}</TabsTrigger>
          <TabsTrigger value="appearance">{t('Appearance')}</TabsTrigger>
        </TabsList>

      <TabsContent value="shortcuts">
        <SettingsSectionHeader title={t('Global Shortcut')} description={t('Global hotkey to start and stop speech recording')} />
        <Card className="settings-card">
          <CardContent className="settings-card-content">
          <div className="setting-row">
            <div className="setting-info">
              <div className="setting-label" id="hotkey-label">{t('Recording Hotkey')}</div>
              <div className="setting-desc">{t('Press this key combination to start/stop voice recording from any app')}</div>
            </div>
            <div className="setting-control">
              <div className="hotkey-recorder">
                <button
                  ref={(el) => {
                    displays.current.hotkey = el;
                  }}
                  type="button"
                  className={`hotkey-display ui-focus-ring${recording === 'hotkey' ? ' recording' : ''}`}
                  id="hotkey-display"
                  aria-pressed={recording === 'hotkey'}
                  aria-labelledby="hotkey-label hotkey-display-text"
                  onClick={() => startRecording('hotkey')}
                  onBlur={() => {
                    if (recordingRef.current === 'hotkey') cancelRecording();
                  }}
                >
                  <span id="hotkey-display-text">
                    {recording === 'hotkey' ? pendingText : hotkey}
                  </span>
                </button>
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
                  {t('Reset')}
                </Button>
              </div>
            </div>
          </div>
          <div className="setting-row">
            <div className="setting-info">
              <div className="setting-label">{t('Transcription Mode Behavior')}</div>
              <div className="setting-desc">{t('How the hotkey controls recording')}</div>
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
                    aria-label={t('Transcription mode behavior')}
                  >
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectGroup>
                      <SelectItem value="push_to_toggle">{t('Push-to-Toggle')}</SelectItem>
                      <SelectItem value="hold_to_record">{t('Hold-to-Record')}</SelectItem>
                    </SelectGroup>
                  </SelectContent>
                </Select>
              </Field>
            </div>
          </div>
          </CardContent>
        </Card>

        <SettingsSectionHeader title={t('Agent Mode Shortcut')} description={t('Dedicated shortcut to activate AI Agent Mode')} />
        <Card className="settings-card">
          <CardContent className="settings-card-content">
          <div className="setting-row">
            <div className="setting-info">
              <div className="setting-label" id="agent-hotkey-label">{t('Agent Mode Hotkey')}</div>
              <div className="setting-desc">{t('Dedicated hotkey to activate AI Agent Mode from any app')}</div>
            </div>
            <div className="setting-control">
              <div className="hotkey-recorder">
                <button
                  ref={(el) => {
                    displays.current.agent_hotkey = el;
                  }}
                  type="button"
                  className={`hotkey-display ui-focus-ring${recording === 'agent_hotkey' ? ' recording' : ''}`}
                  id="agent-hotkey-display"
                  aria-pressed={recording === 'agent_hotkey'}
                  aria-labelledby="agent-hotkey-label agent-hotkey-display-text"
                  onClick={() => startRecording('agent_hotkey')}
                  onBlur={() => {
                    if (recordingRef.current === 'agent_hotkey') cancelRecording();
                  }}
                >
                  <span id="agent-hotkey-display-text">
                    {recording === 'agent_hotkey' ? pendingText : agentHotkey}
                  </span>
                </button>
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
                  {t('Reset')}
                </Button>
              </div>
            </div>
          </div>
          <div className="setting-row">
            <div className="setting-info">
              <div className="setting-label">{t('Agent Mode Behavior')}</div>
              <div className="setting-desc">{t('How the agent hotkey controls recording')}</div>
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
                    aria-label={t('Agent mode behavior')}
                  >
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectGroup>
                      <SelectItem value="push_to_toggle">{t('Push-to-Toggle')}</SelectItem>
                      <SelectItem value="hold_to_record">{t('Hold-to-Record')}</SelectItem>
                    </SelectGroup>
                  </SelectContent>
                </Select>
              </Field>
            </div>
          </div>
          </CardContent>
        </Card>
      </TabsContent>

      <TabsContent value="audio">
        <SettingsSectionHeader title={t('Audio & Language')} description={t('Select input hardware and speech recognition language')} />
        <Card className="settings-card">
          <CardContent className="settings-card-content">
          <div className="setting-row">
            <div className="setting-info">
              <div className="setting-label">{t('Microphone')}</div>
              <div className="setting-desc">{t('Select the audio input device for voice recording')}</div>
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
                    aria-label={t('Microphone input device')}
                  >
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectGroup>
                      <SelectItem value={SYSTEM_DEFAULT_DEVICE}>{t('System Default')}</SelectItem>
                      {devices.map((name) => (
                        <SelectItem key={name} value={name}>{name}</SelectItem>
                      ))}
                      {audioDevice && !devices.includes(audioDevice) && (
                        <SelectItem value={audioDevice}>{audioDevice}</SelectItem>
                      )}
                    </SelectGroup>
                  </SelectContent>
                </Select>
              </Field>
            </div>
          </div>
          <div className="setting-row">
            <div className="setting-info">
              <div className="setting-label">{t('Transcription Language')}</div>
              <div className="setting-desc">{t('Hint to the STT model about the spoken language')}</div>
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
                    aria-label={t('Transcription language')}
                  >
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectGroup>
                      <SelectItem value="en">{t('English')}</SelectItem>
                      <SelectItem value="auto">{t('Auto-detect')}</SelectItem>
                      <SelectItem value="es">{t('Spanish')}</SelectItem>
                      <SelectItem value="fr">{t('French')}</SelectItem>
                      <SelectItem value="de">{t('German')}</SelectItem>
                      <SelectItem value="zh">{t('Chinese')}</SelectItem>
                      <SelectItem value="ja">{t('Japanese')}</SelectItem>
                      <SelectItem value="hi">{t('Hindi')}</SelectItem>
                      <SelectItem value="ar">{t('Arabic')}</SelectItem>
                      <SelectItem value="pt">{t('Portuguese')}</SelectItem>
                      <SelectItem value="it">{t('Italian')}</SelectItem>
                      <SelectItem value="nl">{t('Dutch')}</SelectItem>
                      <SelectItem value="ko">{t('Korean')}</SelectItem>
                      <SelectItem value="ru">{t('Russian')}</SelectItem>
                      <SelectItem value="mr">{t('Marathi')}</SelectItem>
                      <SelectItem value="pa">{t('Punjabi')}</SelectItem>
                      <SelectItem value="hu">{t('Hungarian')}</SelectItem>
                    </SelectGroup>
                  </SelectContent>
                </Select>
              </Field>
            </div>
          </div>
          <div className="setting-row">
            <div className="setting-info">
              <div className="setting-label">{t('Play Completion Sound')}</div>
              <div className="setting-desc">{t('Play a short chime when a transcription finishes')}</div>
            </div>
            <div className="setting-control">
              <Field>
                <Switch
                  id="sound-on-complete-cb"
                  aria-label={t('Play a sound when transcription completes')}
                  checked={sound}
                  onCheckedChange={bindCheck(setSound, 'sound_on_complete')}
                />
              </Field>
            </div>
          </div>
          <div className="setting-row">
            <div className="setting-info">
              <div className="setting-label">{t('Copy Transcription to Clipboard')}</div>
              <div className="setting-desc">{t('Your dictated text and agent results stay on the clipboard after each dictation, even if the paste fails, so you can paste them again with Ctrl+V. Turn this off to keep your earlier copied text instead.')}</div>
            </div>
            <div className="setting-control">
              <Field>
                <Switch
                  id="copy-transcript-cb"
                  aria-label={t('Keep dictated text on the clipboard')}
                  checked={copyTranscript}
                  onCheckedChange={bindCheck(setCopyTranscript, 'copy_transcript_to_clipboard')}
                />
              </Field>
            </div>
          </div>
          </CardContent>
        </Card>
      </TabsContent>

      <TabsContent value="language">
        <SettingsSectionHeader
          title={t('Interface Language')}
          description={t('Choose the language used across the Fluence settings window, wizard, and overlay')}
        />
        <Card className="settings-card">
          <CardContent className="settings-card-content">
          <div className="setting-row">
            <div className="setting-info">
              <div className="setting-label">{t('Display Language')}</div>
              <div className="setting-desc">{t('Changes apply immediately. System default follows your Windows language.')}</div>
            </div>
            <div className="setting-control">
              <Field>
                <Select
                  value={uiLanguage}
                  onValueChange={(value) => void onUiLanguageChange(value)}
                >
                  <SelectTrigger
                    id="ui-language-select"
                    className="select-md"
                    aria-label={t('Interface language')}
                  >
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectGroup>
                      <SelectItem value={SYSTEM_LANGUAGE}>
                        {t('System default')}{resolvedSystemLanguage ? ` (${resolvedSystemLanguage})` : ''}
                      </SelectItem>
                      {UI_LANGUAGES.map((language) => (
                        <SelectItem key={language.code} value={language.code}>
                          {language.nativeName}
                        </SelectItem>
                      ))}
                    </SelectGroup>
                  </SelectContent>
                </Select>
              </Field>
            </div>
          </div>
          <div className="setting-row">
            <div className="setting-desc" style={{ margin: 0 }}>
              {t('The settings window reloads briefly after a language change. Transcription language stays separate and is configured under Audio.')}
            </div>
          </div>
          </CardContent>
        </Card>
      </TabsContent>

      <TabsContent value="system">
        <SettingsSectionHeader title={t('System Preferences')} description={t('Configure startup and background audio behavior')} />
        <Card className="settings-card">
          <CardContent className="settings-card-content">
          <div className="setting-row">
            <div className="setting-info">
              <div className="setting-label">{t('Launch at Windows Startup')}</div>
              <div className="setting-desc">{t('Automatically start Fluence when you log in to Windows')}</div>
            </div>
            <div className="setting-control">
              <Field>
                <Switch
                  id="autostart-cb"
                  aria-label={t('Launch at Windows startup')}
                  checked={autostart}
                  onCheckedChange={bindCheck(setAutostart, 'auto_start', 'autostart')}
                />
              </Field>
            </div>
          </div>
          <div className="setting-row">
            <div className="setting-info">
              <div className="setting-label">{t('Mute Background Apps')}</div>
              <div className="setting-desc">{t('Silence music, videos, and calls while you dictate')}</div>
            </div>
            <div className="setting-control">
              <Field>
                <Switch
                  id="duck-cb"
                  aria-label={t('Mute background apps while dictating')}
                  checked={duck}
                  onCheckedChange={bindCheck(setDuck, 'duck_enabled')}
                />
              </Field>
            </div>
          </div>
          <div className="setting-row">
            <div className="setting-info">
              <div className="setting-label">{t('Grab Highlighted Text')}</div>
              <div className="setting-desc">{t('Automatically read highlighted text when entering Agent Mode')}</div>
            </div>
            <div className="setting-control">
              <Field>
                <Switch
                  id="auto-grab-cb"
                  aria-label={t('Grab highlighted text when entering Agent Mode')}
                  checked={autoGrab}
                  onCheckedChange={bindCheck(setAutoGrab, 'auto_grab_highlight')}
                />
              </Field>
            </div>
          </div>
          </CardContent>
        </Card>
      </TabsContent>
      <TabsContent value="appearance">
        <SettingsSectionHeader title={t('Accent theme')} description={t('Choose a preset accent for the Fluence settings window')} />
        <Card className="settings-card">
          <CardContent className="settings-card-content accent-theme-content">
            <RadioGroup
              className="accent-preset-grid"
              value={accentTheme}
              onValueChange={(value) => onAccentThemeChange(value as AccentTheme)}
              aria-label={t('Accent theme')}
              aria-describedby="accent-theme-note"
            >
              {ACCENT_THEMES.map((preset) => (
                <RadioGroupItem
                  key={preset.value}
                  asChild
                  value={preset.value}
                  className="accent-preset-radio"
                  style={{
                    '--preset-accent-dark': preset.dark,
                    '--preset-accent-light': preset.light,
                  } as CSSProperties}
                >
                  <button type="button" className="accent-preset-card">
                    <span className="accent-preset-swatch" aria-hidden="true" />
                    <span className="accent-preset-label">{t(preset.label)}</span>
                  </button>
                </RadioGroupItem>
              ))}
            </RadioGroup>
            <p className="accent-theme-note" id="accent-theme-note">
              {t('Accents adapt for dark and light appearance. Your surfaces, Fluence logo, and floating overlay stay unchanged.')}
            </p>
          </CardContent>
        </Card>
      </TabsContent>
      </Tabs>

      {tab !== 'appearance' && (
        <div className="page-actions">
          <Button variant="default" id="save-general-btn" onClick={() => void onSaveAll()}>{t('Save Changes')}</Button>
        </div>
      )}
    </section>
  );
}
