import { useEffect, useState } from 'react';
import { Button } from '@/components/ui/button';
import { Field } from '@/components/ui/field';
import { RadioGroup, RadioGroupItem } from '@/components/ui/radio-group';
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
import { toast } from '@/components/fluence/Toasts';
import { SettingsSectionHeader } from '@/components/fluence/SettingsSection';
import { OverlayPreview, type OverlayTier } from '@/components/fluence/OverlayPreview';
import {
  getCachedSettings,
  loadSettings,
  saveSettingsNow,
  setSettingField,
  subscribeSettings,
} from '@/ipc/settings';

type OverlayStyle = OverlayTier;

const STYLE_OPTIONS: { value: OverlayTier; title: string; desc: string }[] = [
  { value: 'full', title: 'Full Status Island', desc: 'Shows the timer, status, and live waveform while you dictate' },
  { value: 'compact', title: 'Compact Pill', desc: 'A slim strip with just the waveform and a close button' },
  { value: 'bubble', title: 'Minimal Bubble', desc: 'A tiny floating dot. Hover it to see the close button' },
];

function str(value: unknown, fallback: string): string {
  return typeof value === 'string' && value ? value : fallback;
}

// Dedicated Floating Bubble studio — Android BubbleSettingsScreen parity,
// rebuilt on the frozen Windows tokens.
// Each style row shows a LIVE preview running the exact production code:
// the real overlay markup (src/overlay.html subtree) styled by the real
// overlay stylesheet (src/css/overlay.css, Shadow-DOM isolated) with a
// frozen production waveform frame (src/js/audio-viz.js draw pass). What
// you see is what the hotkey summons — no approximations to drift.
// All writes go through the existing setSettingField debounced pipeline,
// so autosave + Ctrl+S flush behave identically to General.
export function BubblePage() {
  const [overlayStyle, setOverlayStyle] = useState<OverlayStyle>('full');
  const [overlayPosition, setOverlayPosition] = useState('bottom_right');
  const [overlayGlow, setOverlayGlow] = useState(true);
  const [showAppPill, setShowAppPill] = useState(true);

  // Style picks the overlay; Appearance tunes how it looks and where it
  // sits. Placement lives with appearance (a one-row tab of its own would
  // feel artificial), and the live previews keep reacting either way.
  type BubbleTab = 'style' | 'appearance';
  const [tab, setTab] = useState<BubbleTab>('style');

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        await loadSettings();
      } catch {
        /* controls keep defaults until settings arrive */
      }
      if (cancelled) return;
      const s = getCachedSettings();
      if (s) {
        setOverlayStyle((str(s.overlay_style, 'full') as OverlayStyle) ?? 'full');
        setOverlayPosition(str(s.overlay_position, 'bottom_right'));
        setOverlayGlow((s.overlay_glow as boolean) !== false);
        setShowAppPill((s.show_app_pill as boolean) !== false);
      }
    })();
    const unsub = subscribeSettings(() => {
      const s = getCachedSettings();
      if (!s) return;
      setOverlayStyle((str(s.overlay_style, 'full') as OverlayStyle) ?? 'full');
      setOverlayPosition(str(s.overlay_position, 'bottom_right'));
      setOverlayGlow((s.overlay_glow as boolean) !== false);
      setShowAppPill((s.show_app_pill as boolean) !== false);
    });
    return () => {
      cancelled = true;
      unsub();
    };
  }, []);

  useEffect(() => {
    const onSave = () => {
      saveSettingsNow().catch((err) => toast('Failed to save settings: ' + String(err), 'error'));
    };
    window.addEventListener('fluence:save-page', onSave);
    return () => window.removeEventListener('fluence:save-page', onSave);
  }, []);

  const onStyleChange = (value: string) => {
    const v = (value as OverlayStyle) ?? 'full';
    setOverlayStyle(v);
    if (getCachedSettings()) {
      setSettingField('overlay_style', v);
    }
  };

  const onPositionChange = (value: string) => {
    setOverlayPosition(value);
    if (getCachedSettings()) {
      setSettingField('overlay_position', value);
    }
  };

  const onGlowChange = (value: boolean) => {
    setOverlayGlow(value);
    if (getCachedSettings()) {
      setSettingField('overlay_glow', value);
    }
  };

  const onAppPillChange = (value: boolean) => {
    setShowAppPill(value);
    if (getCachedSettings()) {
      setSettingField('show_app_pill', value);
    }
  };

  const onSaveAll = async () => {
    try {
      await saveSettingsNow();
    } catch (err) {
      toast('Failed to save settings: ' + String(err), 'error');
      return;
    }
    toast('Settings saved', 'success');
  };

  return (
    <section className="page active" id="page-bubble">
      <div className="page-header">
        <h1 className="page-title" tabIndex={-1}>
          Floating Bubble
        </h1>
        <p className="page-subtitle">Preview each style live, then set the appearance and placement</p>
      </div>

      <Tabs className="page-tabs" value={tab} onValueChange={(v) => setTab(v as BubbleTab)}>
        <TabsList aria-label="Floating bubble area">
          <TabsTrigger value="style">Style</TabsTrigger>
          <TabsTrigger value="appearance">Appearance</TabsTrigger>
        </TabsList>

      <TabsContent value="style">
        <SettingsSectionHeader
          title="Overlay Style"
          description="Choose the floating recording indicator that appears on screen"
        />
        <div className="settings-card">
          <RadioGroup
            className="bubble-option-list"
            value={overlayStyle}
            onValueChange={onStyleChange}
            aria-label="Overlay style"
          >
            {STYLE_OPTIONS.map((option) => {
              const selected = overlayStyle === option.value;
              return (
                <div
                  key={option.value}
                  className={`choice-surface selection-row bubble-option${selected ? ' selected' : ''}`}
                  onClick={() => onStyleChange(option.value)}
                >
                  <RadioGroupItem value={option.value} asChild>
                    <button
                      type="button"
                      className="selection-row-main"
                      aria-label={`Select ${option.title}`}
                    >
                      <span className="selection-radio" aria-hidden="true" />
                      <span className="selection-row-copy">
                        <span className="selection-row-heading">
                          <span className="selection-row-title">{option.title}</span>
                        </span>
                        <span className="selection-row-description">{option.desc}</span>
                      </span>
                    </button>
                  </RadioGroupItem>
                  <div className="bubble-live-stage" onClick={(e) => e.stopPropagation()}>
                    <OverlayPreview tier={option.value} glowOn={overlayGlow} position={overlayPosition} pillOn={showAppPill} />
                  </div>
                </div>
              );
            })}
          </RadioGroup>
        </div>
        <div className="bubble-legend">
          <span className="bubble-legend-item">
            <span className="bubble-legend-dot bubble-legend-dot-t" aria-hidden="true" />
            Transcription mode
          </span>
          <span className="bubble-legend-item">
            <span className="bubble-legend-dot bubble-legend-dot-a" aria-hidden="true" />
            Agent mode
          </span>
        </div>
      </TabsContent>

      <TabsContent value="appearance">
        <SettingsSectionHeader
          title="Appearance"
          description="Visual indicators and glow effects for dictation"
        />
        <div className="settings-card">
          <div className="setting-row">
            <div className="setting-info">
              <div className="setting-label">Halo Glow</div>
              <div className="setting-desc">Adds a soft glow around the overlay. Purple while you dictate, teal in Agent mode</div>
            </div>
            <div className="setting-control">
              <Field>
                <Switch
                  id="bubble-glow-cb"
                  aria-label="Overlay halo glow"
                  checked={overlayGlow}
                  onCheckedChange={onGlowChange}
                />
              </Field>
            </div>
          </div>
          <div className="setting-row">
            <div className="setting-info">
              <div className="setting-label">App Pill</div>
              <div className="setting-desc">Shows the name and icon of the app you are dictating into</div>
            </div>
            <div className="setting-control">
              <Field>
                <Switch
                  id="bubble-app-pill-cb"
                  aria-label="Foreground app pill"
                  checked={showAppPill}
                  onCheckedChange={onAppPillChange}
                />
              </Field>
            </div>
          </div>
        </div>

        <SettingsSectionHeader
          title="Placement"
          description="Where the floating overlay appears on screen during recording"
        />
        <div className="settings-card">
          <div className="setting-row">
            <div className="setting-info">
              <div className="setting-label">Overlay Position</div>
              <div className="setting-desc">Where the floating overlay appears on screen during recording</div>
            </div>
            <div className="setting-control">
              <Field>
                <Select value={overlayPosition} onValueChange={onPositionChange}>
                  <SelectTrigger id="bubble-position-select" className="select-md" aria-label="Overlay position">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectGroup>
                      <SelectItem value="bottom_right">Bottom Right</SelectItem>
                      <SelectItem value="bottom_left">Bottom Left</SelectItem>
                      <SelectItem value="center">Center Bottom</SelectItem>
                      <SelectItem value="top_left">Top Left</SelectItem>
                      <SelectItem value="top_center">Top Center</SelectItem>
                      <SelectItem value="top_right">Top Right</SelectItem>
                    </SelectGroup>
                  </SelectContent>
                </Select>
              </Field>
            </div>
          </div>
        </div>
      </TabsContent>
      </Tabs>

      <div className="page-actions">
        <Button variant="default" id="save-bubble-btn" onClick={() => void onSaveAll()}>Save Changes</Button>
      </div>
    </section>
  );
}
