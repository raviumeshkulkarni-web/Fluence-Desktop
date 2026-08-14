/**
 * Fluence Windows — Siri-Style Canvas Waveform Visualizer
 * 
 * Android-inspired stateless visualization behavior using the existing
 * Windows RMS telemetry.
 * 
 * Three layered sine waves with parabolic edge envelope, phase-integrated
 * scrolling, and amplitude-driven frequency vibration.
 * 
 * Purple (amethyst) = transcription mode
 * Cyan (agent)      = agent mode
 */

class AuraVisualizer {
  constructor(canvasId) {
    this.canvas = document.getElementById(canvasId);
    this.ctx = this.canvas?.getContext('2d');
    this.overlayRoot = document.getElementById('overlay-root');
    this.currentState = 'idle';
    this.smoothedAmplitude = 0;      // 0.0 – 1.0
    this.phase = 0;                  // integrated phase (radians)
    this.recordingStartTime = 0;
    this.recordingFramesReceived = 0;
    this.lastTime = null;
    this.lastAmplitudeAt = 0;
    this._lastFrameAt = 0;
    this.noiseFloor = 1.0;   // fast-attack/slow-release min tracker (seeded high)
    this.noiseSpread = 0.005;
    this.calibrationFrames = 0;
    this._rafId = null;
    this._isRunning = false;
    this._resize();

    if (window.__TAURI__?.event?.listen) {
      window.__TAURI__.event.listen('window-visibility', (evt) => {
        if (evt.payload === true) {
          this._isRunning = true;
          if (this._rafId === null) this._loop(performance.now());
        } else {
          this._isRunning = false;
          if (this._rafId !== null) {
            cancelAnimationFrame(this._rafId);
            this._rafId = null;
            this.lastTime = null;
          }
          // Never let the draw loop die mid-recording: if a stray visibility
          // event lands while audio is live, resume immediately so the meter
          // keeps moving instead of latching a frozen frame.
          if (this.currentState === 'recording' || this.currentState === 'agent') {
            this._isRunning = true;
            this._loop(performance.now());
          }
        }
      });
    } else {
      this._isRunning = true;
      this._loop(performance.now());
    }

    window.addEventListener('resize', () => this._resize());

    // Self-healing watchdog: if the rAF loop ever stalls while recording
    // (WebView2 occlusion/frame throttling, dropped visibility events, or a
    // draw exception that kills the rAF chain), restart it so the waveform
    // can never freeze for the rest of the session.
    setInterval(() => {
      const isActive = this.currentState === 'recording' || this.currentState === 'agent';
      if (!isActive) return;
      this._isRunning = true;
      const now = performance.now();
      const stalled =
        this._rafId === null ||
        (this._lastFrameAt > 0 && now - this._lastFrameAt > 1000);
      if (stalled) {
        if (this._rafId !== null) cancelAnimationFrame(this._rafId);
        this._rafId = null;
        this.lastTime = null;
        this._loop(now);
      }
    }, 500);
  }

  _resize() {
    if (!this.canvas) return;
    const dpr = window.devicePixelRatio || 1;
    // offsetWidth/offsetHeight are transform-independent (unlike
    // getBoundingClientRect), so the entry scale animation cannot trigger
    // backing-store resets. The rounded device-pixel comparison also skips
    // resizes when the layout size only changes fractionally.
    const cssW = this.canvas.offsetWidth;
    const cssH = this.canvas.offsetHeight;
    if (cssW === 0 || cssH === 0) return;
    const w = Math.max(1, Math.round(cssW * dpr));
    const h = Math.max(1, Math.round(cssH * dpr));
    if (w === this.canvas.width && h === this.canvas.height) return;
    this.canvas.width = w;
    this.canvas.height = h;
    if (this.ctx) {
      this.ctx.setTransform(1, 0, 0, 1, 0, 0);
      this.ctx.scale(dpr, dpr);
    }
    this._logicalW = cssW;
    this._logicalH = cssH;
  }

  setAmplitude(rawAmplitude) {
    if (this.currentState !== 'recording' && this.currentState !== 'agent') {
      return;
    }

    this.recordingFramesReceived++;
    this.lastAmplitudeAt = performance.now();

    // Safety sanitize input in [0.0, 1.0]
    const raw = Math.max(0, Math.min(Number(rawAmplitude) || 0, 1.0));

    // Noise-floor tracker (frontend-only; the Rust audio pipeline is frozen).
    // The backend reports an absolute dBFS-derived level in [0,1], so the
    // same physical silence can sit at ~0.0 on a clean mic or ~0.7 on a
    // boosted input (100% mic gain, equalizer, virtual device). A fixed
    // offset can't work, but a floor seeded at 0 that may only drift inside
    // a deadband anchored to itself can never climb into a mid-scale noise
    // band — every noise fluctuation would render as waveform motion.
    //
    // This mirrors how TypeWhisper keeps its meter calm on boosted inputs
    // (raw levels gated below their noise floor), adapted to this
    // dB-compressed domain: a fast-attack, slow-release minimum tracker
    // seeds the floor at the machine's true noise bottom within ~2 s, then a
    // statistical gate sizes the deadband to the noise's own fluctuation
    // (spread) so jittery high-gain noise stays flat while real speech reads.
    this.calibrationFrames++;
    const gate = Math.min(Math.max(2.5 * this.noiseSpread + 0.01, 0.02), 0.15);
    const canSnap = this.calibrationFrames <= 60 || raw > this.noiseFloor - 0.25;
    if (raw < this.noiseFloor && canSnap) {
      // Fast attack on new lows (calibration permits full snaps; afterwards
      // a >0.25 one-frame drop is treated as a mute/dropout glitch, not a
      // floor change).
      this.noiseFloor = raw;
    } else if (raw >= this.noiseFloor && raw < this.noiseFloor + gate) {
      // Quiet band above the floor: slow-release the floor toward the
      // ambient level and track the noise's fluctuation. Speech (raw well
      // above the band) freezes both, so a long continuous utterance can
      // never raise the floor and flatten the meter.
      this.noiseFloor += (raw - this.noiseFloor) * 0.005;
      this.noiseSpread = this.noiseSpread * 0.9 + Math.abs(raw - this.noiseFloor) * 0.1;
    }
    this.noiseSpread = Math.max(0.005, Math.min(this.noiseSpread, 0.12));

    // Statistical gate: hide everything within ~2.5x the noise fluctuation
    // above the floor (plus a small epsilon). The deadband auto-sizes to the
    // machine's actual noise, so the meter stays flat at any input volume.
    const threshold = this.noiseFloor
      + Math.min(Math.max(2.5 * this.noiseSpread + 0.01, 0.02), 0.15);

    // Relative meter: how far the current level is above the machine's own
    // noise floor, scaled to the remaining headroom.
    let rel = (raw - threshold) / Math.max(0.05, 1.0 - threshold);
    rel = Math.max(0, Math.min(rel, 1));

    // Perceptual sqrt curve (same approach as the reference dictation apps):
    // compresses residual floor motion toward zero while keeping quiet
    // speech visible.
    let normalized = Math.sqrt(rel);

    // Asymmetric EMA smoothing (fast 0.50 attack, smooth 0.15 elastic decay)
    const prev = this.smoothedAmplitude;
    if (normalized > prev) {
      this.smoothedAmplitude = prev * 0.50 + normalized * 0.50;
    } else {
      this.smoothedAmplitude = prev * 0.85 + normalized * 0.15;
    }
    if (this.smoothedAmplitude < 0.001) {
      this.smoothedAmplitude = 0;
    }
    this.smoothedAmplitude = Math.max(0, Math.min(this.smoothedAmplitude, 1.0));
  }

  setState(state) {
    if (this.currentState === state) return;
    this.currentState = state;

    if (!this.overlayRoot) return;
    // Only manage the state-* classes. Wiping className here would destroy
    // the tier style (style-full/compact/bubble) and corner docking classes
    // applied by overlay.js on every state transition — the overlay would
    // snap back to the full-size card and the waveform canvas would jump.
    this.overlayRoot.classList.remove(
      'state-idle',
      'state-recording',
      'state-agent',
      'state-transcribing',
      'state-agent_transcribing',
      'state-success',
      'state-error'
    );
    if (state !== 'idle') this.overlayRoot.classList.add(`state-${state}`);

    if (state === 'idle' || state === 'transcribing' || state === 'agent_transcribing') {
      this.smoothedAmplitude = 0;
      this.recordingStartTime = 0;
      this.recordingFramesReceived = 0;
      this.lastAmplitudeAt = 0;
      this.noiseFloor = 1.0;
      this.noiseSpread = 0.005;
      this.calibrationFrames = 0;
    } else if (state === 'recording' || state === 'agent') {
      this.recordingStartTime = performance.now();
      this.recordingFramesReceived = 0;
      this.smoothedAmplitude = 0;
      this.lastAmplitudeAt = 0;
      this.noiseFloor = 1.0;
      this.noiseSpread = 0.005;
      this.calibrationFrames = 0;
      if (this._rafId === null) {
        this._isRunning = true;
        this._loop(performance.now());
      }
    }
  }

  getState() { return this.currentState; }

  _loop(timestamp) {
    if (!this._isRunning && this.currentState === 'idle') {
      this._rafId = null;
      return;
    }

    this._rafId = requestAnimationFrame((t) => this._loop(t));

    if (this.lastTime === null) { this.lastTime = timestamp; }
    const dt = Math.min((timestamp - this.lastTime) / 1000, 0.1);
    this.lastTime = timestamp;

    if (this.currentState === 'idle') {
      const heartbeatPeriod = 4000;
      const heartbeatPhase = (timestamp % heartbeatPeriod) / heartbeatPeriod;
      const heartbeatPulse = Math.sin(heartbeatPhase * Math.PI) * 0.08;
      this.smoothedAmplitude = heartbeatPulse;
    } else if (this.currentState === 'recording' || this.currentState === 'agent') {
      // If amplitude events stall (emit drops, event-loop hiccup), settle the
      // meter toward a calm baseline instead of latching a stale frame.
      const sinceEvent = timestamp - this.lastAmplitudeAt;
      if (sinceEvent > 400) {
        this.smoothedAmplitude *= 0.88;
        if (this.smoothedAmplitude < 0.001) this.smoothedAmplitude = 0;
      }
    }

    const speed = 0.60 + this.smoothedAmplitude * 1.20;
    this.phase = (this.phase + speed * dt * 2 * Math.PI) % (1000 * Math.PI);
    this._lastFrameAt = timestamp;

    try {
      this._draw();
    } catch (err) {
      // A canvas/gradient exception must never kill the animation loop.
      console.warn('Waveform draw error:', err);
    }
  }

  _draw() {
    const ctx = this.ctx;
    if (!ctx || !this.canvas) return;

    const cssW = this.canvas.offsetWidth;
    const cssH = this.canvas.offsetHeight;
    if (cssW !== this._logicalW || cssH !== this._logicalH || this.canvas.width === 0) {
      this._resize();
    }

    const W = this._logicalW || 160;
    const H = this._logicalH || 44;

    if (this.currentState === 'idle') {
      ctx.clearRect(0, 0, W, H);
      return;
    }

    ctx.clearRect(0, 0, W, H);

    ctx.lineCap = 'round';
    ctx.lineJoin = 'round';

    const isAgent = this.currentState === 'agent' || this.currentState === 'agent_transcribing';

    const primaryColor = isAgent ? '#00F5D4' : '#B08AC8';
    const forefrontColor = isAgent ? '#E6FFFA' : '#F1EAF5';
    const primaryAlpha = 0.45;

    const centerY = H / 2;
    // Active amplitude: baseline 10% up to 45% of total height
    const activeAmplitude = (this.smoothedAmplitude * 0.85 + 0.10) * (H * 0.45);

    const phase1 = this.phase;
    const phase2 = -this.phase * 0.7;

    const env = (x) => Math.sin((x / W) * Math.PI);

    // 1px sampling on tiny canvases keeps the curve smooth where a 2px step
    // would alias into jagged edges.
    const step = W < 60 ? 1 : 2;

    // Ripple scroll rate matches its carrier wave on any canvas width. The
    // spatial frequencies stay in absolute pixels (0.1 / 0.15 / 0.08 rad/px,
    // the original calm ripple shapes), while the width-derived phase
    // multipliers make the ripple advance at the carrier's rate — otherwise
    // the ripple outruns the wave on narrow canvases (perceived as churn).
    const vibPhase1 = (0.1 * W) / (2 * Math.PI * 1.5);
    const vibPhase2 = (0.15 * W) / (2 * Math.PI * 2.5);
    const vibPhase3 = (0.08 * W) / (2 * Math.PI * 1.2);

    // Wave 1: background wave (color: primary, alpha ~0.45)
    ctx.beginPath();
    ctx.moveTo(0, centerY);
    for (let x = 0; x <= W; x += step) {
      const e = env(x);
      const angle = (x / W) * 2 * Math.PI * 1.5 + phase1;
      const vibration = Math.sin(x * 0.1 + phase1 * vibPhase1) * this.smoothedAmplitude * 4;
      const y = centerY + (Math.sin(angle) * activeAmplitude * 0.5 + vibration) * e;
      ctx.lineTo(x, y);
    }
    const grad1 = ctx.createLinearGradient(0, 0, W, 0);
    grad1.addColorStop(0, 'transparent');
    grad1.addColorStop(0.5, hexAlpha(primaryColor, primaryAlpha));
    grad1.addColorStop(1, 'transparent');
    ctx.strokeStyle = grad1;
    ctx.lineWidth = 1.5;
    ctx.stroke();

    // Wave 2: middle wave (slightly higher freq, bolder)
    ctx.beginPath();
    ctx.moveTo(0, centerY);
    for (let x = 0; x <= W; x += step) {
      const e = env(x);
      const angle = (x / W) * 2 * Math.PI * 2.5 + phase2;
      const vibration = Math.sin(x * 0.15 - phase2 * vibPhase2) * this.smoothedAmplitude * 3;
      const y = centerY + (Math.sin(angle) * activeAmplitude * 0.7 + vibration) * e;
      ctx.lineTo(x, y);
    }
    const grad2 = ctx.createLinearGradient(0, 0, W, 0);
    grad2.addColorStop(0, 'transparent');
    grad2.addColorStop(0.5, hexAlpha(primaryColor, primaryAlpha));
    grad2.addColorStop(1, 'transparent');
    ctx.strokeStyle = grad2;
    ctx.lineWidth = 1.8;
    ctx.stroke();

    // Wave 3: forefront wave (brightest, most visible)
    ctx.beginPath();
    ctx.moveTo(0, centerY);
    for (let x = 0; x <= W; x += step) {
      const e = env(x);
      const angle = (x / W) * 2 * Math.PI * 1.2 + (phase1 - phase2) * 0.5;
      const vibration = Math.sin(x * 0.08 + (phase1 - phase2) * 0.5 * vibPhase3) * this.smoothedAmplitude * 5;
      const y = centerY + (Math.sin(angle) * activeAmplitude * 0.9 + vibration) * e;
      ctx.lineTo(x, y);
    }
    const grad3 = ctx.createLinearGradient(0, 0, W, 0);
    grad3.addColorStop(0, 'transparent');
    grad3.addColorStop(0.5, hexAlpha(forefrontColor, 0.95));
    grad3.addColorStop(1, 'transparent');
    ctx.strokeStyle = grad3;
    ctx.lineWidth = 2.2;
    ctx.stroke();
  }
}

function hexAlpha(hex, alpha) {
  const r = parseInt(hex.slice(1, 3), 16);
  const g = parseInt(hex.slice(3, 5), 16);
  const b = parseInt(hex.slice(5, 7), 16);
  return `rgba(${r},${g},${b},${alpha})`;
}

window.AuraVisualizer = AuraVisualizer;
