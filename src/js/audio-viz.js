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
        }
      });
    } else {
      this._isRunning = true;
      this._loop(performance.now());
    }

    window.addEventListener('resize', () => this._resize());
  }

  _resize() {
    if (!this.canvas) return;
    const dpr = window.devicePixelRatio || 1;
    const rect = this.canvas.getBoundingClientRect();
    if (rect.width === 0 || rect.height === 0) return;
    this.canvas.width = rect.width * dpr;
    this.canvas.height = rect.height * dpr;
    if (this.ctx) {
      this.ctx.setTransform(1, 0, 0, 1, 0, 0);
      this.ctx.scale(dpr, dpr);
    }
    this._logicalW = rect.width;
    this._logicalH = rect.height;
  }

  setAmplitude(rawAmplitude) {
    if (this.currentState !== 'recording' && this.currentState !== 'agent') {
      return;
    }

    this.recordingFramesReceived++;

    // Safety sanitize input in [0.0, 1.0]
    const raw = Math.max(0, Math.min(Number(rawAmplitude) || 0, 1.0));

    // Stateless mapping of widened Windows RMS telemetry (spanning -75 dBFS to -30 dBFS).
    // Silence/ambient sits around 0.0 - 0.05.
    // Conversational & sustained speech sits around 0.35 - 0.75.
    // Loud speech reaches 0.85 - 1.00.
    let normalized = Math.max(0, Math.min((raw - 0.02) * 1.25, 1.0));

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
    this.overlayRoot.className = 'overlay-root';
    if (state !== 'idle') this.overlayRoot.classList.add(`state-${state}`);

    if (state === 'idle' || state === 'transcribing' || state === 'agent_transcribing') {
      this.smoothedAmplitude = 0;
      this.recordingStartTime = 0;
      this.recordingFramesReceived = 0;
    } else if (state === 'recording' || state === 'agent') {
      this.recordingStartTime = performance.now();
      this.recordingFramesReceived = 0;
      this.smoothedAmplitude = 0;
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
    }

    const speed = 0.60 + this.smoothedAmplitude * 1.20;
    this.phase = (this.phase + speed * dt * 2 * Math.PI) % (1000 * Math.PI);

    this._draw();
  }

  _draw() {
    const ctx = this.ctx;
    if (!ctx || !this.canvas) return;

    const rect = this.canvas.getBoundingClientRect();
    if (rect.width !== this._logicalW || rect.height !== this._logicalH || this.canvas.width === 0) {
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

    // Wave 1: background wave (color: primary, alpha ~0.45)
    ctx.beginPath();
    ctx.moveTo(0, centerY);
    for (let x = 0; x <= W; x += 2) {
      const e = env(x);
      const angle = (x / W) * 2 * Math.PI * 1.5 + phase1;
      const vibration = Math.sin(x * 0.1 + phase1 * 3) * this.smoothedAmplitude * 4;
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
    for (let x = 0; x <= W; x += 2) {
      const e = env(x);
      const angle = (x / W) * 2 * Math.PI * 2.5 + phase2;
      const vibration = Math.sin(x * 0.15 - phase2 * 4) * this.smoothedAmplitude * 3;
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
    for (let x = 0; x <= W; x += 2) {
      const e = env(x);
      const angle = (x / W) * 2 * Math.PI * 1.2 + (phase1 - phase2) * 0.5;
      const vibration = Math.sin(x * 0.08 + phase1 * 5) * this.smoothedAmplitude * 5;
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
