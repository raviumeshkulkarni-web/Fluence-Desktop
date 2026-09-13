import { useEffect, useState, type KeyboardEvent } from 'react';
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
    }
    toast('Settings saved', 'success');
  };

  const onOptionKeyDown = (e: KeyboardEvent<HTMLDivElement>, index: number) => {
    const current = STYLE_OPTIONS[index]?.value;
    if (!current) return;
    if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      onStyleChange(current);
      return;
    }
    let next: number | null = null;
    if (e.key === 'ArrowDown' || e.key === 'ArrowRight') next = (index + 1) % STYLE_OPTIONS.length;
    if (e.key === 'ArrowUp' || e.key === 'ArrowLeft') {
      next = (index - 1 + STYLE_OPTIONS.length) % STYLE_OPTIONS.length;
    }
    if (next !== null) {
      e.preventDefault();
      e.currentTarget.parentElement
        ?.querySelectorAll<HTMLElement>('.bubble-option')[next]?.focus();
    }
  };

  return (
    <section className="page active" id="page-bubble">
      <div className="page-header">
        <h1 className="page-title" tabIndex={-1}>
          Floating Bubble
        </h1>
        <p className="page-subtitle">Preview each style live, then set the appearance and placement</p>
      </div>

      <div className="settings-section">
        <div className="settings-section-header">
          <h2>Overlay Style</h2>
        </div>
        <div role="radiogroup" aria-label="Overlay style">
          {STYLE_OPTIONS.map((option, index) => (
            <div
              key={option.value}
              className="bubble-option"
              role="radio"
              aria-checked={overlayStyle === option.value}
              aria-label={option.title}
              tabIndex={0}
              onClick={() => onStyleChange(option.value)}
              onKeyDown={(e) => onOptionKeyDown(e, index)}
            >
              <span className="bubble-radio" aria-hidden="true" />
              <span className="bubble-option-text">
                <span className="bubble-preview-label">{option.title}</span>
                <span className="bubble-preview-desc">{option.desc}</span>
              </span>
              <span className="bubble-live-stage">
                <OverlayPreview tier={option.value} glowOn={overlayGlow} position={overlayPosition} pillOn={showAppPill} />
              </span>
            </div>
          ))}
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
      </div>

      <div className="settings-section">
        <div className="settings-section-header">
          <h2>Appearance</h2>
        </div>
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

      <div className="settings-section">
        <div className="settings-section-header">
          <h2>Placement</h2>
        </div>
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
                  <SelectItem value="bottom_right">Bottom Right</SelectItem>
                  <SelectItem value="bottom_left">Bottom Left</SelectItem>
                  <SelectItem value="center">Center Bottom</SelectItem>
                  <SelectItem value="top_left">Top Left</SelectItem>
                  <SelectItem value="top_center">Top Center</SelectItem>
                  <SelectItem value="top_right">Top Right</SelectItem>
                </SelectContent>
              </Select>
            </Field>
          </div>
        </div>
      </div>

      <div style={{ display: 'flex', justifyContent: 'flex-end', paddingTop: 'var(--spacing-md)' }}>
        <Button variant="primary" id="save-bubble-btn" style={{ minWidth: 120 }} onClick={() => void onSaveAll()}>Save Changes</Button>
      </div>
    </section>
  );
}
