import type { TranslationKey } from '@/i18n';

// Mirror of the six AuraVisualizer paint passes (src/js/audio-viz.js),
// following the drawFrozenWaveform precedent in OverlayPreview.tsx: the
// settings window may not instantiate the overlay's rAF loop, but what the
// picker shows must be the exact production shapes. Keep every constant
// here in sync with audio-viz.js (same mirror license as drawFrozenWaveform).

export type WaveformStyle = 'waves' | 'bars';

// Same fallback contract as AuraVisualizer.setStyle: unknown or missing
// values normalize back to "waves", so a hand-edited settings file can
// never break the picker or the draw loop.
export function normalizeWaveformStyle(value: unknown): WaveformStyle {
  return value === 'bars' ? 'bars' : 'waves';
}

export const WAVEFORM_STYLE_OPTIONS: { value: WaveformStyle; title: TranslationKey; desc: TranslationKey }[] = [
  { value: 'waves', title: 'Waves', desc: 'Three layered ripples. The animation Fluence ships with' },
  { value: 'bars', title: 'Amp Bars', desc: 'Equalizer bars that track how loud you speak' },
];

// Full-tier factors, mirrored from _styleFactors (audio-viz.js) with the
// transcription-mode paints (preview never shows agent cyan).
const PRIMARY = '#8B45D8';
const FOREFRONT = '#F1EAF5';
const PRIMARY_ALPHA = 0.32;
const LW_SCALE = 1;
const BUBBLE_SCALE = 1;

interface StyleFrames {
  // Production meter: 6% calm baseline, 40% height headroom, tier bubble
  // scale (same as _styleFactors). Takes the canvas height because the
  // meter is absolute pixels, like production.
  base(level: number, H: number): number;
}

const meters: StyleFrames = {
  base(level: number, H: number): number {
    return (level * 0.88 + 0.06) * (H * 0.4) * BUBBLE_SCALE;
  },
};

function hexAlpha(hex: string, alpha: number): string {
  const r = parseInt(hex.slice(1, 3), 16);
  const g = parseInt(hex.slice(3, 5), 16);
  const b = parseInt(hex.slice(5, 7), 16);
  return `rgba(${r},${g},${b},${alpha})`;
}

// One frozen representative frame for the picker rows that do not animate.
// Fixed phase + level (drawFrozenWaveform precedent). The waves branch is
// the exact drawFrozenWaveform trio; the other rows mirror their
// production passes one-to-one. Colors stay the shipped transcription pair
// (amethyst / foreground wash): the accent does not follow the waveform.
export function drawWaveformFrame(
  canvas: HTMLCanvasElement,
  style: WaveformStyle,
  phase: number,
  level: number,
): void {
  const dpr = window.devicePixelRatio || 1;
  const cssW = canvas.offsetWidth;
  const cssH = canvas.offsetHeight;
  if (cssW === 0 || cssH === 0) return;
  const w = Math.max(1, Math.round(cssW * dpr));
  const h = Math.max(1, Math.round(cssH * dpr));
  if (canvas.width !== w || canvas.height !== h) {
    canvas.width = w;
    canvas.height = h;
  }
  const ctx = canvas.getContext('2d');
  if (!ctx) return;
  ctx.setTransform(1, 0, 0, 1, 0, 0);
  ctx.scale(dpr, dpr);
  ctx.clearRect(0, 0, cssW, cssH);
  ctx.lineCap = 'round';
  ctx.lineJoin = 'round';

  const W = cssW;
  const H = cssH;
  const centerY = H / 2;
  const glow = level > 0.28 ? level * 7 : 0;
  const altGrad = (hex: string, alpha: number) => {
    const grad = ctx.createLinearGradient(0, 0, W, 0);
    grad.addColorStop(0, hexAlpha(hex, alpha * 0.22));
    grad.addColorStop(0.5, hexAlpha(hex, alpha));
    grad.addColorStop(1, hexAlpha(hex, alpha * 0.22));
    return grad;
  };

  if (style === 'waves') {
    // Exact drawFrozenWaveform trio (production wave 1/2/3).
    const activeAmplitude = (level * 0.88 + 0.06) * (H * 0.4);
    const env = (x: number) => Math.pow(Math.sin((x / W) * Math.PI), 1.22);
    const step = W < 60 ? 1 : 2;
    const phase1 = phase;
    const phase2 = -phase * 0.7;
    const vibPhase1 = (0.1 * W) / (2 * Math.PI * 1.5);
    const vibPhase2 = (0.15 * W) / (2 * Math.PI * 2.5);
    const vibPhase3 = (0.08 * W) / (2 * Math.PI * 1.2);
    const vib = level > 0.04 ? level : 0;
    const trace = (
      freq: number,
      ph: number,
      scale: number,
      jitter: number,
      vibRate: number,
      vibGain: number,
      stroke: string,
      lineWidth: number,
    ) => {
      ctx.beginPath();
      ctx.moveTo(0, centerY);
      for (let x = 0; x <= W; x += step) {
        const e = env(x);
        const angle = (x / W) * 2 * Math.PI * freq + ph;
        const vibration = Math.sin(x * vibRate + ph * jitter) * vib * vibGain;
        const y = centerY + (Math.sin(angle) * activeAmplitude * scale + vibration) * e;
        ctx.lineTo(x, y);
      }
      const grad = ctx.createLinearGradient(0, 0, W, 0);
      grad.addColorStop(0, 'transparent');
      grad.addColorStop(0.5, stroke);
      grad.addColorStop(1, 'transparent');
      ctx.strokeStyle = grad;
      ctx.lineWidth = lineWidth * LW_SCALE;
      ctx.stroke();
    };

    trace(1.5, phase1, 0.48, vibPhase1, 0.1, 2.0, hexAlpha(PRIMARY, PRIMARY_ALPHA), 1.35);
    trace(2.5, phase2, 0.68, vibPhase2, 0.15, 1.45, hexAlpha(PRIMARY, PRIMARY_ALPHA * 1.08), 1.65);

    if (glow > 0) {
      ctx.shadowColor = hexAlpha(FOREFRONT, 0.22);
      ctx.shadowBlur = glow;
    }
    trace(1.2, (phase1 - phase2) * 0.5, 0.88, vibPhase3, 0.08, 2.4, hexAlpha(FOREFRONT, 0.9), 1.9);
    ctx.shadowBlur = 0;
    ctx.shadowColor = 'transparent';
    return;
  }

  if (style === 'bars') {
    // Mirror of _drawBars: soft primary bed + crisp forefront row gated
    // by the real level.
    const count = W < 60 ? 7 : Math.min(25, Math.max(9, Math.floor(W / 6)));
    const edgeFloor = 0.22;
    const edge = (x: number) => { const s = Math.pow(Math.sin((x / W) * Math.PI), 0.7); return edgeFloor + (1 - edgeFloor) * s; };
    const clampH = (h: number) => Math.min(h, H * 0.48);
    ctx.strokeStyle = altGrad(PRIMARY, PRIMARY_ALPHA * 1.05);
    ctx.lineWidth = 2.4;
    for (let i = 0; i < count; i++) {
      const x = ((i + 0.5) / count) * W;
      const e = edge(x);
      const u1 = Math.abs(Math.sin(x * (W < 60 ? 0.16 : 0.11) + phase * 1.35));
      const u2 = Math.abs(Math.sin(x * (W < 60 ? 0.24 : 0.17) - phase * 1.05));
      const hBg = clampH((meters.base(level, H) * (0.34 + 0.52 * Math.min(1, u1 * 0.62 + u2 * 0.38)) + 1.2) * e);
      ctx.beginPath();
      ctx.moveTo(x, centerY - hBg);
      ctx.lineTo(x, centerY + hBg);
      ctx.stroke();
    }
    ctx.strokeStyle = altGrad(FOREFRONT, 0.88);
    ctx.lineWidth = 1.9;
    if (glow > 0) {
      ctx.shadowColor = hexAlpha(FOREFRONT, 0.22);
      ctx.shadowBlur = glow;
    }
    for (let i = 0; i < count; i++) {
      const x = ((i + 0.5) / count) * W;
      const e = edge(x);
      const u1 = Math.abs(Math.sin(x * (W < 60 ? 0.16 : 0.11) + phase * 1.35));
      const u2 = Math.abs(Math.sin(x * (W < 60 ? 0.24 : 0.17) - phase * 1.05));
      const shape = Math.min(1, u1 * 0.62 + u2 * 0.38);
      const hFt = clampH((meters.base(level, H) * (0.10 + 0.62 * shape * Math.max(0.25, level)) + 1.6) * e);
      ctx.beginPath();
      ctx.moveTo(x, centerY - hFt);
      ctx.lineTo(x, centerY + hFt);
      ctx.stroke();
    }
    ctx.shadowBlur = 0;
    ctx.shadowColor = 'transparent';
    return;
  }

}
