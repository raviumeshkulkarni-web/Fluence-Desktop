import { useEffect, useState, type CSSProperties } from 'react';
import { Card, CardContent } from '@/components/ui/card';
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
import { HotkeyRecorderField } from '@/components/fluence/HotkeyRecorderField';
import { PageHeader } from '@/components/fluence/PageHeader';
import { SettingRow } from '@/components/fluence/SettingRow';
import { toast } from '@/components/fluence/Toasts';
import { SettingsSectionHeader } from '@/components/fluence/SettingsSection';
import {
  getCachedSettings,
  loadSettings,
  saveSettingsNow,
  setSettingField,
  subscribeSettings,
} from '@/ipc/settings';
import {
  DEFAULT_AGENT_HOTKEY,
  DEFAULT_HOTKEY,
  initGeneralApply,
  listAudioDevices,
  persistGeneral,
} from '@/ipc/general';
import { ACCENT_THEMES, type AccentTheme } from '@/lib/theme';
import { describeError } from '@/lib/errors';

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
  // Opt-in: command words ("period") also occur in normal speech.
  const [spokenFormatting, setSpokenFormatting] = useState(false);

  // One concern visible at a time (Providers pattern): shortcuts for the
  // hotkeys, audio for input/language/feedback, system for OS behavior.
  type GeneralTab = 'shortcuts' | 'audio' | 'system' | 'appearance';
  const [tab, setTab] = useState<GeneralTab>('shortcuts');

  const [recording, setRecording] = useState<HotkeyKey | null>(null);

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
        setAutostart(s.auto_start === true);
        setDuck(s.duck_enabled === true);
        setAutoGrab(s.auto_grab_highlight !== false);
        setSound((s.sound_on_complete as boolean) ?? true);
        setCopyTranscript(s.copy_transcript_to_clipboard ?? true);
        setSpokenFormatting(s.spoken_formatting_enabled === true);
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

    // The tray can change the recording mode from another window; keep the
    // visible controls in sync with the canonical cached settings.
    const unsubscribe = subscribeSettings(() => {
      if (cancelled) return;
      const s = getCachedSettings();
      if (!s) return;
      setRecordingMode(str(s.recording_mode, 'push_to_toggle'));
      setAgentRecordingMode(str(s.agent_recording_mode, 'push_to_toggle'));
    });

    return () => {
      cancelled = true;
      unsubscribe();
    };
  }, []);

  // Ctrl+S quiet save (vanilla saveGeneral alias: flush, no toast).
  useEffect(() => {
    const onSave = () => {
      saveSettingsNow().catch((err) =>
        toast('Could not save settings. ' + describeError(err), 'error'),
      );
    };
    window.addEventListener('fluence:save-page', onSave);
    return () => window.removeEventListener('fluence:save-page', onSave);
  }, []);

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

  return (
    <section className="page active" id="page-general">
      <PageHeader
        title="General"
        subtitle="Hotkey, dictation, audio, and system preferences"
      />

      <Tabs className="page-tabs" value={tab} onValueChange={(v) => setTab(v as GeneralTab)}>
        <TabsList aria-label="General settings area">
          <TabsTrigger value="shortcuts">Shortcuts</TabsTrigger>
          <TabsTrigger value="audio">Dictation</TabsTrigger>
          <TabsTrigger value="system">System</TabsTrigger>
          <TabsTrigger value="appearance">Appearance</TabsTrigger>
        </TabsList>

        <TabsContent value="shortcuts">
          <SettingsSectionHeader title="Global Shortcut" description="Global hotkey to start and stop speech recording" />
          <Card className="settings-card">
            <CardContent className="settings-card-content">
              <SettingRow
                label="Recording Hotkey"
                labelId="hotkey-label"
                description="Press this key combination to start/stop voice recording from any app"
              >
                <HotkeyRecorderField
                  value={hotkey}
                  isRecording={recording === 'hotkey'}
                  labelId="hotkey-label"
                  displayId="hotkey-display"
                  displayTextId="hotkey-display-text"
                  clearBtnId="hotkey-clear-btn"
                  onStartRecording={() => setRecording('hotkey')}
                  onStopRecording={() => setRecording((prev) => (prev === 'hotkey' ? null : prev))}
                  onCommit={(val) => {
                    setHotkey(val);
                    if (getCachedSettings()) {
                      setSettingField('hotkey', val);
                      persistGeneral('hotkeys');
                    }
                  }}
                  onReset={() => {
                    setHotkey(DEFAULT_HOTKEY);
                    if (getCachedSettings()) {
                      setSettingField('hotkey', DEFAULT_HOTKEY);
                      persistGeneral('hotkeys');
                    }
                  }}
                />
              </SettingRow>
              <SettingRow
                label="Transcription Mode Behavior"
                htmlFor="recording-mode-select"
                description="How the hotkey controls recording"
              >
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
                    <SelectGroup>
                      <SelectItem value="push_to_toggle">Push-to-Toggle</SelectItem>
                      <SelectItem value="hold_to_record">Hold-to-Record</SelectItem>
                    </SelectGroup>
                  </SelectContent>
                </Select>
              </SettingRow>
            </CardContent>
          </Card>

          <SettingsSectionHeader title="Agent Mode Shortcut" description="Dedicated shortcut to activate AI Agent Mode" />
          <Card className="settings-card">
            <CardContent className="settings-card-content">
              <SettingRow
                label="Agent Mode Hotkey"
                labelId="agent-hotkey-label"
                description="Dedicated hotkey to activate AI Agent Mode from any app"
              >
                <HotkeyRecorderField
                  value={agentHotkey}
                  isRecording={recording === 'agent_hotkey'}
                  labelId="agent-hotkey-label"
                  displayId="agent-hotkey-display"
                  displayTextId="agent-hotkey-display-text"
                  clearBtnId="agent-hotkey-clear-btn"
                  onStartRecording={() => setRecording('agent_hotkey')}
                  onStopRecording={() => setRecording((prev) => (prev === 'agent_hotkey' ? null : prev))}
                  onCommit={(val) => {
                    setAgentHotkey(val);
                    if (getCachedSettings()) {
                      setSettingField('agent_hotkey', val);
                      persistGeneral('hotkeys');
                    }
                  }}
                  onReset={() => {
                    setAgentHotkey(DEFAULT_AGENT_HOTKEY);
                    if (getCachedSettings()) {
                      setSettingField('agent_hotkey', DEFAULT_AGENT_HOTKEY);
                      persistGeneral('hotkeys');
                    }
                  }}
                />
              </SettingRow>
              <SettingRow
                label="Agent Mode Behavior"
                htmlFor="agent-recording-mode-select"
                description="How the agent hotkey controls recording"
              >
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
                    <SelectGroup>
                      <SelectItem value="push_to_toggle">Push-to-Toggle</SelectItem>
                      <SelectItem value="hold_to_record">Hold-to-Record</SelectItem>
                    </SelectGroup>
                  </SelectContent>
                </Select>
              </SettingRow>
            </CardContent>
          </Card>
        </TabsContent>

        <TabsContent value="audio">
          <SettingsSectionHeader title="Audio & Language" description="Select input hardware and speech recognition language" />
          <Card className="settings-card">
            <CardContent className="settings-card-content">
              <SettingRow
                label="Microphone"
                htmlFor="audio-device-select"
                description="Select the audio input device for voice recording"
              >
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
                    <SelectGroup>
                      <SelectItem value={SYSTEM_DEFAULT_DEVICE}>System Default</SelectItem>
                      {devices.map((name) => (
                        <SelectItem key={name} value={name}>{name}</SelectItem>
                      ))}
                      {audioDevice && !devices.includes(audioDevice) && (
                        <SelectItem value={audioDevice}>{audioDevice}</SelectItem>
                      )}
                    </SelectGroup>
                  </SelectContent>
                </Select>
              </SettingRow>
              <SettingRow
                label="Transcription Language"
                htmlFor="language-select"
                description="Hint to the STT model about the spoken language"
              >
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
                    <SelectGroup>
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
                    </SelectGroup>
                  </SelectContent>
                </Select>
              </SettingRow>
              <SettingRow
                label="Play Completion Sound"
                htmlFor="sound-on-complete-cb"
                description="Play a short chime when a transcription finishes"
              >
                <Switch
                  id="sound-on-complete-cb"
                  aria-label="Play a sound when transcription completes"
                  checked={sound}
                  onCheckedChange={bindCheck(setSound, 'sound_on_complete')}
                />
              </SettingRow>
              <SettingRow
                label="Copy Transcription to Clipboard"
                htmlFor="copy-transcript-cb"
                description="Your dictated text and agent results stay on the clipboard after each dictation, even if the paste fails, so you can paste them again with Ctrl+V. Turn this off to keep your earlier copied text instead."
              >
                <Switch
                  id="copy-transcript-cb"
                  aria-label="Keep dictated text on the clipboard"
                  checked={copyTranscript}
                  onCheckedChange={bindCheck(setCopyTranscript, 'copy_transcript_to_clipboard')}
                />
              </SettingRow>
            </CardContent>
          </Card>

          <SettingsSectionHeader
            title="Spoken Formatting"
            description="Turn spoken command words into punctuation and line breaks"
          />
          <Card className="settings-card">
            <CardContent className="settings-card-content">
              <SettingRow
                label="Spoken Punctuation"
                htmlFor="spoken-formatting-cb"
                description="Say &quot;comma&quot;, &quot;period&quot;, &quot;question mark&quot;, &quot;exclamation mark&quot;, &quot;colon&quot;, &quot;semicolon&quot;, &quot;new line&quot;, or &quot;new paragraph&quot; to format the transcript. Off by default because these words also appear in normal speech."
              >
                <Switch
                  id="spoken-formatting-cb"
                  aria-label="Convert spoken punctuation commands"
                  checked={spokenFormatting}
                  onCheckedChange={bindCheck(
                    setSpokenFormatting,
                    'spoken_formatting_enabled',
                  )}
                />
              </SettingRow>
            </CardContent>
          </Card>
        </TabsContent>

        <TabsContent value="system">
          <SettingsSectionHeader title="System Preferences" description="Configure startup and background audio behavior" />
          <Card className="settings-card">
            <CardContent className="settings-card-content">
              <SettingRow
                label="Launch at Windows Startup"
                htmlFor="autostart-cb"
                description="Automatically start Fluence when you log in to Windows"
              >
                <Switch
                  id="autostart-cb"
                  aria-label="Launch at Windows startup"
                  checked={autostart}
                  onCheckedChange={bindCheck(setAutostart, 'auto_start', 'autostart')}
                />
              </SettingRow>
              <SettingRow
                label="Mute Background Apps"
                htmlFor="duck-cb"
                description="Silence music, videos, and calls while you dictate"
              >
                <Switch
                  id="duck-cb"
                  aria-label="Mute background apps while dictating"
                  checked={duck}
                  onCheckedChange={bindCheck(setDuck, 'duck_enabled')}
                />
              </SettingRow>
              <SettingRow
                label="Grab Highlighted Text"
                htmlFor="auto-grab-cb"
                description="Automatically read highlighted text when entering Agent Mode"
              >
                <Switch
                  id="auto-grab-cb"
                  aria-label="Grab highlighted text when entering Agent Mode"
                  checked={autoGrab}
                  onCheckedChange={bindCheck(setAutoGrab, 'auto_grab_highlight')}
                />
              </SettingRow>
            </CardContent>
          </Card>
        </TabsContent>

        <TabsContent value="appearance">
          <SettingsSectionHeader title="Accent theme" description="Choose a preset accent for the Fluence settings window" />
          <Card className="settings-card">
            <CardContent className="settings-card-content accent-theme-content">
              <RadioGroup
                className="accent-preset-grid"
                value={accentTheme}
                onValueChange={(value) => onAccentThemeChange(value as AccentTheme)}
                aria-label="Accent theme"
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
                      <span className="accent-preset-label">{preset.label}</span>
                    </button>
                  </RadioGroupItem>
                ))}
              </RadioGroup>
              <p className="accent-theme-note" id="accent-theme-note">
                Accents adapt for dark and light appearance. Your surfaces, Fluence logo, and floating overlay stay unchanged.
              </p>
            </CardContent>
          </Card>
        </TabsContent>
      </Tabs>

      <div className="page-actions page-actions-autosave">
        <p className="autosave-hint">Changes save automatically.</p>
      </div>
    </section>
  );
}
