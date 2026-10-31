import { useEffect, useRef } from 'react';
import { drawWaveformFrame, type WaveformStyle } from './waveform';

// One live waveform meter for the picker rows, running the mirrored
// production paints (waveform.ts). Motion budget rule: only the SELECTED
// row animates; every other row shows a frozen representative frame (fixed
// phase + level, OverlayPreview precedent). prefers-reduced-motion freezes
// every row. The synthetic level is a speech-like envelope so the pick
// shows how the style reacts to talking without needing the real mic.
export function WaveformStage({
  style,
  animate,
}: {
  style: WaveformStyle;
  animate: boolean;
}) {
  const canvasRef = useRef<HTMLCanvasElement>(null);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const motionQuery = window.matchMedia('(prefers-reduced-motion: reduce)');
    let raf = 0;
    let lastTime: number | null = null;
    let phase = 0.6;

    const livingLevel = (t: number): number => {
      // Two slow, incommensurate sinusoids layered like a syllabic
      // envelope; stays inside [0.16, 0.74] so the pick never looks dead
      // or maxed out.
      const a = 0.5 + 0.5 * Math.sin(t * 2.2);
      const b = 0.6 + 0.4 * Math.sin(t * 3.4 + 1.1);
      return Math.max(0.16, Math.min(0.74, 0.16 + 0.58 * a * b));
    };

    const loop = (timestamp: number): void => {
      raf = requestAnimationFrame(loop);
      if (lastTime === null) lastTime = timestamp;
      const dt = Math.min((timestamp - lastTime) / 1000, 0.1);
      lastTime = timestamp;
      const level = livingLevel(timestamp / 1000);
      // Same phase integrator as the production _loop (speed follows the
      // meter), so hooks like the orb's speak-quicker breathing carry over.
      phase = (phase + (0.48 + level * 0.82) * dt * 2 * Math.PI) % (1000 * Math.PI);
      drawWaveformFrame(canvas, style, phase, level);
    };

    const stopAndFreeze = (): void => {
      if (raf) cancelAnimationFrame(raf);
      raf = 0;
      lastTime = null;
      phase = 0.6;
      drawWaveformFrame(canvas, style, 0.6, 0.35);
    };

    if (animate && !motionQuery.matches) {
      raf = requestAnimationFrame(loop);
    } else {
      stopAndFreeze();
    }

    const onMotionChange = (): void => {
      if (!animate) return;
      if (motionQuery.matches && raf) stopAndFreeze();
      else if (!motionQuery.matches && !raf) raf = requestAnimationFrame(loop);
    };
    motionQuery.addEventListener?.('change', onMotionChange);

    return () => {
      if (raf) cancelAnimationFrame(raf);
      raf = 0;
      motionQuery.removeEventListener?.('change', onMotionChange);
    };
  }, [style, animate]);

  return (
    <canvas
      ref={canvasRef}
      className="wave-stage-canvas"
      aria-hidden="true"
    />
  );
}
