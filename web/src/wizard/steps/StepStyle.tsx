import { useRef } from 'react';
import type { KeyboardEvent } from 'react';
import { OverlayPreview, type OverlayTier } from '@/components/fluence/OverlayPreview';
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
  const groupRef = useRef<HTMLDivElement>(null);

  const onGroupKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    const buttons = Array.from(
      groupRef.current?.querySelectorAll<HTMLButtonElement>('.wiz-style-option') ?? [],
    );
    if (!buttons.length) return;
    const current = buttons.indexOf(document.activeElement as HTMLButtonElement);
    let next: number | null = null;
    if (e.key === 'ArrowRight' || e.key === 'ArrowDown') {
      e.preventDefault();
      next = (current + 1 + buttons.length) % buttons.length;
    } else if (e.key === 'ArrowLeft' || e.key === 'ArrowUp') {
      e.preventDefault();
      next = (current - 1 + buttons.length) % buttons.length;
    } else {
      return;
    }
    const target = buttons[next];
    target?.focus();
    target?.click();
  };

  return (
    <>
      <h1 className="step-title" tabIndex={-1}>
        Choose Your Overlay
      </h1>
      <p className="step-desc">
        This is what appears when you press your hotkey. Pick the look you prefer.
      </p>
      <div className="step-content">
        <div
          ref={groupRef}
          role="radiogroup"
          aria-label="Overlay style"
          className="wiz-style-list"
          onKeyDown={onGroupKeyDown}
        >
          {STYLE_OPTIONS.map((option) => (
            <button
              key={option.value}
              type="button"
              role="radio"
              aria-checked={data.overlayStyle === option.value}
              aria-label={option.title}
              className={`wiz-style-option${
                data.overlayStyle === option.value ? ' selected' : ''
              }`}
              onClick={() => onPatch({ overlayStyle: option.value })}
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
          ))}
        </div>
      </div>
    </>
  );
}
