import { useEffect, useRef } from 'react';
import overlayCssRaw from '../../../../src/css/overlay.css?raw';
import tokensCssRaw from '../../../../src/css/design-tokens.css?raw';
import { drawWaveformFrame, type WaveformStyle } from './waveform';

export type OverlayTier = 'full' | 'compact' | 'bubble';

const CORNER_CLASSES = ['corner-bottom-left', 'corner-bottom-right', 'corner-bottom-center', 'corner-top-left', 'corner-top-right', 'corner-top-center'] as const;

// Exact mirror of setOverlayCorner (src/js/overlay.js) — including the
// else-branch fallthrough to corner-bottom-center, so unknown values can
// never drift between product and preview.
function cornerClassFor(position: string): string {
  if (position === 'bottom_left') return 'corner-bottom-left';
  if (position === 'bottom_right') return 'corner-bottom-right';
  if (position === 'top_left') return 'corner-top-left';
  if (position === 'top_right') return 'corner-top-right';
  if (position === 'top_center') return 'corner-top-center';
  return 'corner-bottom-center';
}
// Exact production stylesheet, inlined at build time. The `@import` line is
// stripped: design tokens + fonts are already on the host document and CSS
// custom properties pierce the shadow boundary, so paints resolve identically.
// Injected into a Shadow Root per preview, so overlay.css page-frame rules
// (`*` reset, `html/body` flex, transparent backdrop) can never leak into the
// settings shell — only `.overlay-root` and friends match, and only in here.
const OVERLAY_CSS = overlayCssRaw.replace(/^@import[^;]+;/m, '');

// Dark token pin (src/css/design-tokens.css `:root` block, re-scoped to the
// preview frame). The real overlay window never sets `data-theme`, so it
// always resolves the dark `:root` values — but a light-mode settings page
// would otherwise bleed its `[data-theme="light"]` tokens through the
// shadow boundary via inheritance, washing the preview out. This restores
// the exact overlay-window environment: dark tokens, both themes.
const rootBlock = tokensCssRaw.match(/:root\s*\{[\s\S]*?\n\}/)?.[0] ?? '';
const DARK_TOKENS_CSS = rootBlock.replace(/:root/, '.ovpv-frame');

// Preview-frame rules only (layout the shadow stage, never the overlay):
// - `.ovpv-frame` replaces overlay.html `body` (flex centering, no 46px
//   top offset — the app-pill is hidden in previews, same as the default
//   product state with no foreground app).
// - `-webkit-app-region: no-drag` so dragging a preview never drags the
//   settings window (production needs drag; the preview must not).
const FRAME_CSS = `
.ovpv-frame {
  position: relative;
  display: flex;
  width: max-content;
  justify-content: center;
  transform: scale(var(--ovpv-scale, 1));
  transform-origin: top left;
}
/* Pill reserve mirrors overlay.html body padding-top:46 — room for the
 * half-docked app pill, taken only while the pill is shown. */
.ovpv-frame.has-pill { padding-top: 46px; }
.ovpv-frame .overlay-root { -webkit-app-region: no-drag; }
`;

// Exact copy of src/overlay.html `#overlay-root` subtree (tags, classes,
// ids, copy), with per-tier root classes. Timer text is demo content —
// production starts at 00:00; 00:07 shows the tabular figures. Inner
// controls are display-only: the host marks the tree `inert` + aria-hidden
// and dead-ends button clicks natively (see below), because overlay.js —
// with its Tauri IPC, rAF loop, and watchdog — is deliberately NOT loaded
// here (same call as Android's static preview: exact paints, zero service
// dependency, no 60fps recomposition inside settings).
function markupFor(tier: OverlayTier, glowOn: boolean, position: string, pillOn: boolean): string {
  const styleClass = tier === 'full' ? '' : ` style-${tier}`;
  // Absence of `.no-glow` means glow ON — same contract as overlay.js, so
  // pre-toggle settings files keep the halo in both product and preview.
  const glowClass = glowOn === false ? ' no-glow' : '';
  const cornerClass = cornerClassFor(position);
  // Demo pill content ("Chrome", blank icon chip): production shows the real
  // foreground app here when one is detected; markup, placement, and
  // typography are exact, only the content is illustrative (same license as
  // the 00:07 demo timer).
  const pillIcon = 'data:image/gif;base64,R0lGODlhAQABAAD/ACwAAAAAAQABAAACADs=';
  const pill = pillOn
    ? `<div class="app-pill visible"><img class="app-pill-icon" alt="" width="16" height="16" src="${pillIcon}" /><span class="app-pill-name">Chrome</span></div>`
    : `<div class="app-pill" hidden><img class="app-pill-icon" alt="" width="16" height="16" src="${pillIcon}" /><span class="app-pill-name"></span></div>`;
  return `
  <div class="ovpv-frame${pillOn ? ' has-pill' : ''}" inert aria-hidden="true">
    ${pill}
    <div class="overlay-root active ${cornerClass}${styleClass}${glowClass}">
      <div class="card-meta">
        <div class="card-meta-left">
          <div class="card-rec">
            <span class="rec-dot"></span>
            <span class="rec-stack">
              <span class="rec-label">LISTENING</span>
              <span class="rec-hint" hidden></span>
            </span>
          </div>
        </div>
        <span class="mode-badge">Transcribe</span>
      </div>
      <div class="card-waveform">
        <canvas id="waveform-canvas"></canvas>
      </div>
      <div class="card-footer">
        <span class="card-timer">00:07</span>
        <button class="card-discard" type="button" tabindex="-1" title="Discard recording" aria-label="Discard recording">
          <svg width="10" height="10" viewBox="0 0 10 10" fill="none" aria-hidden="true"><path d="M2 2l6 6M8 2L2 8" stroke="currentColor" stroke-width="1.4" stroke-linecap="round"/></svg>
        </button>
      </div>
      <div class="status-icon success-icon" aria-hidden="true">
        <svg width="20" height="20" viewBox="0 0 16 16" fill="none">
          <path d="M3.3335 8.00016L6.66683 11.3335L13.3335 4.66683" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"/>
        </svg>
      </div>
      <div class="status-icon error-icon" aria-hidden="true">
        <svg width="20" height="20" viewBox="0 0 16 16" fill="none">
          <path d="M4 4L12 12M12 4L4 12" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"/>
        </svg>
      </div>
      <div class="status-msg" role="status" aria-live="polite">Inserted</div>
      <button class="status-retry" type="button" aria-label="Retry" tabindex="-1" hidden>
        <svg class="status-retry-icon" width="12" height="12" viewBox="0 0 16 16" fill="none" aria-hidden="true">
          <path d="M13 5.5A5.5 5.5 0 1 0 13.5 8M13 2.5v3h-3" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"/>
        </svg>
        <span class="status-retry-label">Retry</span>
      </button>
    </div>
  </div>`;
}

// Demo app icon for the preview pill: true Chromemark geometry (big red top
// cap with horizontal right divider, yellow right wedge, green left wedge,
// white ring + blue hub) as explicit annular-sector paths — no dash tiling
// to drift. Production shows the real foreground-app icon via
// get_foreground_app_icon; only this illustrative stand-in ships here,
// encoded at runtime so no manual URI-escaping can drift.
const CHROME_SVG = `<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 16 16'><circle cx='8' cy='8' r='7' fill='#fff'/><path d='M1.51,5.38 A7 7 0 0 1 14.99,7.76 L10.9,7.9 A2.9 2.9 0 0 0 5.31,6.91 Z' fill='#EA4335'/><path d='M14.99,8.24 A7 7 0 0 1 6.43,14.82 L7.35,10.83 A2.9 2.9 0 0 0 10.9,8.1 Z' fill='#FBBC05'/><path d='M5.95,14.69 A7 7 0 0 1 1.34,5.84 L5.24,7.1 A2.9 2.9 0 0 0 7.15,10.77 Z' fill='#34A853'/><circle cx='8' cy='8' r='3' fill='#fff'/><circle cx='8' cy='8' r='2' fill='#4285F4'/></svg>`;

// Live-fidelity preview of one overlay tier: the real overlay markup styled
// by the real overlay stylesheet, with a frozen production waveform frame.
// Display-only (inert): selecting happens on the surrounding radio row.
export function OverlayPreview({
  tier,
  glowOn = true,
  position = 'bottom_right',
  pillOn = false,
  waveformStyle = 'waves',
}: {
  tier: OverlayTier;
  glowOn?: boolean;
  position?: string;
  pillOn?: boolean;
  waveformStyle?: WaveformStyle;
}) {
  const hostRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const host = hostRef.current;
    if (!host) return;
    // tier is fixed per instance; StrictMode double-mount reuses the root.
    let shadow = host.shadowRoot;
    if (!shadow) {
      shadow = host.attachShadow({ mode: 'open' });
      const style = document.createElement('style');
      style.textContent = DARK_TOKENS_CSS + OVERLAY_CSS + FRAME_CSS;
      const frame = document.createElement('div');
      frame.innerHTML = markupFor(tier, glowOn, position, pillOn);
      shadow.append(style, frame);
      shadow.addEventListener('click', (e) => {
        if ((e.target as Element | null)?.closest?.('button')) e.preventDefault();
      });
    }
    const root = shadow.querySelector('.overlay-root');
    // Glow + corner follow their live prefs with targeted class updates —
    // same remove-then-add order as overlay.js, no rebuild needed.
    root?.classList.toggle('no-glow', glowOn === false);
    if (root) {
      root.classList.remove(...CORNER_CLASSES);
      root.classList.add(cornerClassFor(position));
    }
    // Pill follows its toggle the same way: visibility + name + frame
    // reserve, no rebuild (and no entry-animation replay). The demo icon is
    // set here (encoded at runtime) rather than in the markup string.
    const frame = shadow.querySelector('.ovpv-frame');
    const pill = shadow.querySelector('.app-pill');
    if (frame && pill) {
      frame.classList.toggle('has-pill', pillOn !== false);
      if (pillOn !== false) {
        pill.removeAttribute('hidden');
        pill.classList.add('visible');
        const icon = pill.querySelector('.app-pill-icon');
        if (icon) {
          icon.setAttribute('src', `data:image/svg+xml;charset=utf-8,${encodeURIComponent(CHROME_SVG)}`);
          icon.setAttribute('alt', 'Chrome icon');
        }
        const name = pill.querySelector('.app-pill-name');
        if (name && !name.textContent) name.textContent = 'Chrome';
      } else {
        pill.setAttribute('hidden', '');
        pill.classList.remove('visible');
      }
    }
    const canvas = shadow.querySelector('canvas');
    if (!canvas) return;
    let raf = 0;
    const draw = () => drawWaveformFrame(canvas, waveformStyle, 0.6, 0.35, tier);
    raf = requestAnimationFrame(draw);
    const ro = 'ResizeObserver' in window ? new ResizeObserver(draw) : null;
    ro?.observe(canvas);
    return () => {
      cancelAnimationFrame(raf);
      ro?.disconnect();
    };
  }, [tier, glowOn, position, pillOn, waveformStyle]);

  return (
    <div
      ref={hostRef}
      className={`overlay-preview overlay-preview-${tier}${pillOn !== false ? ' has-pill' : ''}`}
    />
  );
}
