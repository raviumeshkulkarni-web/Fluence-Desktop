import { OverlayPreview, type OverlayTier } from '@/components/fluence/OverlayPreview';
import { RadioGroup, RadioGroupItem } from '@/components/ui/radio-group';
import type { WizardData } from '../Wizard';

interface StepStyleProps {
  data: WizardData;
  onPatch: (patch: Partial<WizardData>) => void;
}

// Same three tiers, titles, and descriptions as Settings → Floating Bubble
// (BubblePage STYLE_OPTIONS). Each row previews the real overlay markup +
// stylesheet (Shadow-DOM isolated, frozen waveform frame), so what you pick
// is what the hotkey summons. Glow on, app pill off: the pill needs a real
// foreground app and would only add height here.
const STYLE_OPTIONS: { value: OverlayTier; title: string; desc: string }[] = [
  { value: 'full', title: 'Full Status Island', desc: 'Timer, status, and live waveform while you dictate' },
  { value: 'compact', title: 'Compact Pill', desc: 'A slim strip with just the waveform and a close button' },
  { value: 'bubble', title: 'Minimal Bubble', desc: 'A tiny floating dot. Hover it to see the close button' },
];

export function StepStyle({ data, onPatch }: StepStyleProps) {
  return (
    <>
      <h1 className="step-title" tabIndex={-1}>
        Choose Your Overlay
      </h1>
      <p className="step-desc">
        This is what appears when you press your hotkey. Pick the look you prefer.
      </p>
      <div className="step-content">
        <RadioGroup
          className="wiz-style-list"
          value={data.overlayStyle}
          onValueChange={(overlayStyle) => onPatch({ overlayStyle: overlayStyle as OverlayTier })}
          aria-label="Overlay style"
        >
          {STYLE_OPTIONS.map((option) => (
            <RadioGroupItem key={option.value} value={option.value} asChild>
              <button
                type="button"
                aria-label={option.title}
                className="choice-surface wiz-style-option"
              >
                <span className="wiz-style-radio" aria-hidden="true" />
                <span className="wiz-style-text">
                  <span className="wiz-style-title">{option.title}</span>
                  <span className="wiz-style-desc">{option.desc}</span>
                </span>
                <span
                  className="wiz-style-stage"
                  data-tier={option.value}
                  aria-hidden="true"
                >
                  <OverlayPreview
                    tier={option.value}
                    glowOn
                    position="center"
                    pillOn={false}
                  />
                </span>
              </button>
            </RadioGroupItem>
          ))}
        </RadioGroup>
      </div>
    </>
  );
}
