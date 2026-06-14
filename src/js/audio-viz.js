/**
 * Fluence Windows — Siri-Style Canvas Waveform Visualizer
 * 
 * Port of the Android SiriWaveform composable (FloatingBubbleUI.kt).
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
    this.noiseFloor = 999.0;
    this.peakAmplitude = 0.002;
    this.recordingStartTime = 0;
    this.recordingFramesReceived = 0;
    this.lastTime = null;
    this._rafId = null;
    this._resize();
    this._loop(performance.now());
    
    window.addEventListener('resize', () => this._resize());
  }

  _resize() {
    if (!this.canvas) return;
    const dpr = window.devicePixelRatio || 1;
    const rect = this.canvas.getBoundingClientRect();
    if (rect.width === 0 || rect.height === 0) return; // window not visible yet
    // Setting width/height resets the canvas transform, so scale is always applied fresh
    this.canvas.width = rect.width * dpr;
    this.canvas.height = rect.height * dpr;
    if (this.ctx) {
      this.ctx.setTransform(1, 0, 0, 1, 0, 0); // reset any accumulated transform
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

    const elapsedMs = this.recordingStartTime ? performance.now() - this.recordingStartTime : 0;

    // The backend zeroes out the first hardware wake-up window. Keep the
    // visualizer pinned briefly too so early device ticks never calibrate range.
    if (elapsedMs < 450 || this.recordingFramesReceived <= 6) {
      this.smoothedAmplitude *= 0.75;
      return;
    }

    rawAmplitude = Math.max(0, Math.min(Number(rawAmplitude) || 0, 1.5));

    // Slight fade-in after the guarded startup window for aesthetic smoothness.
    let scale = 1.0;
    if (this.recordingFramesReceived <= 18) {
      scale = (this.recordingFramesReceived - 6) / 12.0;
    }

    // Track noise floor (min)
    if (rawAmplitude < this.noiseFloor) {
      this.noiseFloor = rawAmplitude;
    } else {
      this.noiseFloor += (rawAmplitude - this.noiseFloor) * 0.001; // drift up very slowly
    }

    // Track peak (max)
    if (rawAmplitude > this.peakAmplitude) {
      this.peakAmplitude = rawAmplitude;
    } else {
      this.peakAmplitude -= (this.peakAmplitude - rawAmplitude) * 0.02; // decay peak faster
    }

    let range = this.peakAmplitude - this.noiseFloor;
    if (range < 0.002) range = 0.002; // cap extreme sensitivity to prevent microscopic hardware pops from spiking

    let normalized = (rawAmplitude - this.noiseFloor) / range;
    if (normalized < 0) normalized = 0;
    
    // Noise gate: ignore the bottom 5% of the dynamic range
    if (normalized < 0.05) {
      normalized = 0;
    }

    // Apply the fade-in scale
    normalized *= scale;

    // Exponential moving average — faster attack (0.4) for snappy reactivity
    // Elastic decay: when amplitude drops, decay slowly for a bouncy, premium feel
    const prevAmplitude = this.smoothedAmplitude;
    if (normalized < prevAmplitude) {
      // Slow decay on drop (elastic feel)
      this.smoothedAmplitude = this.smoothedAmplitude * 0.92 + normalized * 0.08;
    } else {
      // Fast attack on rise
      this.smoothedAmplitude = this.smoothedAmplitude * 0.6 + normalized * 0.4;
    }
  }

  setState(state) {
    if (this.currentState === state) return;
    this.currentState = state;
    if (!this.overlayRoot) return;
    this.overlayRoot.className = 'overlay-root';
    if (state !== 'idle') this.overlayRoot.classList.add(`state-${state}`);
    if (state === 'idle' || state === 'transcribing' || state === 'agent_transcribing') {
      this.smoothedAmplitude = 0;
      this.peakAmplitude = 0.002;
      this.noiseFloor = 999.0;
      this.recordingStartTime = 0;
      this.recordingFramesReceived = 0;
    } else if (state === 'recording' || state === 'agent') {
      this.recordingStartTime = performance.now();
      this.recordingFramesReceived = 0;
    }
  }

  getState() { return this.currentState; }

  _loop(timestamp) {
    this._rafId = requestAnimationFrame((t) => this._loop(t));

    if (this.lastTime === null) { this.lastTime = timestamp; }
    const dt = Math.min((timestamp - this.lastTime) / 1000, 0.1); // cap at 100ms
    this.lastTime = timestamp;

    // Idle heartbeat — subtle periodic pulse
    if (this.currentState === 'idle') {
      const heartbeatPeriod = 4000; // 4 seconds
      const heartbeatPhase = (timestamp % heartbeatPeriod) / heartbeatPeriod;
      const heartbeatPulse = Math.sin(heartbeatPhase * Math.PI) * 0.08;
      this.smoothedAmplitude = heartbeatPulse;
    }

    // Phase integration: speed increases with amplitude (matches Android)
    const speed = 1.0 + this.smoothedAmplitude * 4.0;
    this.phase = (this.phase + speed * dt * 2 * Math.PI) % (1000 * Math.PI);

    this._draw();
  }

  _draw() {
    const ctx = this.ctx;
    if (!ctx || !this.canvas) return;

    // Dynamically adjust to canvas bounding rect changes (especially when shown from hidden)
    const rect = this.canvas.getBoundingClientRect();
    if (rect.width !== this._logicalW || rect.height !== this._logicalH || this.canvas.width === 0) {
      this._resize();
    }

    const W = this._logicalW || 160;
    const H = this._logicalH || 44;

    // Clear canvas in idle state — no wave rendering when idle
    if (this.currentState === 'idle') {
      ctx.clearRect(0, 0, W, H);
      this._prevImageData = null;
      return;
    }

    // Motion trail: draw previous frame at low opacity before clearing
    if (this._prevImageData) {
      ctx.putImageData(this._prevImageData, 0, 0);
      ctx.globalAlpha = 0.15;
      ctx.clearRect(0, 0, W, H);
      ctx.globalAlpha = 1.0;
    }

    // Store current frame for next trail effect
    this._prevImageData = ctx.getImageData(0, 0, this.canvas.width, this.canvas.height);

    ctx.clearRect(0, 0, W, H);

    // Smooth line rendering
    ctx.lineCap = 'round';
    ctx.lineJoin = 'round';

    const isAgent = this.currentState === 'agent' || this.currentState === 'agent_transcribing';
    const isActive = this.currentState === 'recording' || this.currentState === 'agent' || this.currentState === 'agent_transcribing';

    // Color palette (matches Android exactly)
    const primaryColor = isAgent ? '#00F5D4' : '#A855F7';
    const forefrontColor = isAgent ? '#E6FFFA' : '#F3E8FF';
    const primaryAlpha = isAgent ? 0.4 : 0.4;

    const centerY = H / 2;
    // Idle: flat 8% of height; active: amplitude-driven up to 48% of height
    const activeAmplitude = (this.smoothedAmplitude * 0.9 + 0.08) * (H * 0.48);

    const phase1 = this.phase;
    const phase2 = -this.phase * 0.7;

    // Helper: parabolic envelope (taper wave to 0 at edges)
    const env = (x) => Math.sin((x / W) * Math.PI);

    // Wave 1: background wave (color: primary, alpha ~0.4)
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
    grad3.addColorStop(0.5, hexAlpha(forefrontColor, 0.9));
    grad3.addColorStop(1, 'transparent');
    ctx.strokeStyle = grad3;
    ctx.lineWidth = 2.0;
    ctx.stroke();
  }
}

function hexAlpha(hex, alpha) {
  // Convert #RRGGBB to rgba(r,g,b,alpha)
  const r = parseInt(hex.slice(1, 3), 16);
  const g = parseInt(hex.slice(3, 5), 16);
  const b = parseInt(hex.slice(5, 7), 16);
  return `rgba(${r},${g},${b},${alpha})`;
}

window.AuraVisualizer = AuraVisualizer;
