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
    this.fastDb = -75.0;         // fast envelope (dBFS)
    this.slowDb = -75.0;         // slow ambient reference (dBFS)
    this.primeBuf = [];          // startup levels for median seeding
    this.gatePrimed = false;
    this.gateOpen = false;
    this.onsetStreak = 0;
    this.stuckStreak = 0;
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

    // Noise gate (frontend-only; the Rust audio pipeline is frozen).
    //
    // The backend maps RMS onto a dBFS-linear value in [0,1] spanning
    // -75…-30 dBFS, so the same physical silence can sit anywhere from
    // ~0.05 (clean laptop mic) to ~0.85 (boosted / AGC input, Windows Mic
    // Boost). Any absolute threshold therefore fails on some machines.
    // Instead we invert the map back to true dBFS and run a dual-time-
    // constant differential meter (the broadcast PPM approach): a fast
    // envelope follows the current level while a slow envelope learns the
    // machine's ambient noise — whatever it is. The meter only opens when
    // the level rises clearly above that ambience reference, so silence
    // stays flat at ANY input volume and AGC hunting/stepping is absorbed
    // as ambience, with zero per-machine tuning and no pipeline changes.
    const db = raw * 45 - 75;
    const DIGITAL_SILENCE_DB = -72; // muted mic / empty-buffer emits
    const SPEECH_TOP_DB = -30;      // top of the backend telemetry window
    const ONSET_DB = 9;
    const CLOSE_DB = 5;

    // Priming (~0.8 s of live telemetry, digital zeros excluded): seed the
    // ambient reference at the lower-quartile startup level so speaking
    // through the first second or a hotkey chime cannot pin the reference
    // to speech. Until primed the meter simply stays calm.
    if (!this.gatePrimed) {
      if (db > DIGITAL_SILENCE_DB) this.primeBuf.push(db);
      if (this.primeBuf.length >= 24) {
        this.primeBuf.sort((a, b) => a - b);
        this.slowDb = this.fastDb = this.primeBuf[this.primeBuf.length >> 2];
        this.gatePrimed = true;
      }
      return;
    }

    // Fast envelope (~60 ms); slow ambient tracker adapts quickly while the
    // gate is closed (τ≈0.6 s, so AGC gain hunting lands on top of it) and
    // freezes while open so utterances never raise it. Real speech always
    // dips back to ambience between words, re-closing the gate; a negligible
    // open-state creep is kept purely as a latch failsafe for hours-long
    // steady machine states. Digital silence (mic muted, empty-buffer
    // dropout) carries no ambience information — feeding it to the tracker
    // would sink the reference and latch the meter open once audio returns,
    // so it is skipped entirely.
    this.fastDb += (db - this.fastDb) * 0.5;
    const live = db > DIGITAL_SILENCE_DB;
    if (!this.gateOpen) {
      if (live) {
        this.slowDb += (this.fastDb - this.slowDb) * 0.055;
        if (this.slowDb > this.fastDb) this.slowDb = this.fastDb;
      }
    } else if (live) {
      this.slowDb += (this.fastDb - this.slowDb) * 0.0005;
    }

    // Hysteresis: open on a sustained ≥9 dB rise above ambience, close when
    // it falls back within 5 dB. The 3-event (~100 ms) onset debounce hides
    // single-frame AGC pump spikes that real speech never produces.
    const diff = this.fastDb - this.slowDb;
    if (!this.gateOpen) {
      if (diff >= ONSET_DB) this.onsetStreak++;
      else this.onsetStreak = 0;
      if (this.onsetStreak >= 3) {
        this.gateOpen = true;
        this.stuckStreak = 0;
      } else {
        // Solid speech through priming can pin slow to speech (all 24 samples
        // loud): if level stays 6dB above ambience for >1s without opening,
        // gently drift ambience down so it eventually opens. Frontend only.
        if (live && diff >= 6) {
          this.stuckStreak++;
          if (this.stuckStreak > 30) {
            this.slowDb -= 0.45;
            if (this.slowDb < -75) this.slowDb = -75;
          }
        } else {
          this.stuckStreak = 0;
        }
      }
    } else if (diff < CLOSE_DB) {
      this.gateOpen = false;
      this.onsetStreak = 0;
      this.stuckStreak = 0;
    } else {
      this.stuckStreak = 0;
    }

    let normalized = 0;
    if (this.gateOpen) {
      // Relative loudness above the ambient reference, scaled to the
      // headroom remaining up to full telemetry scale.
      const refDb = this.slowDb + ONSET_DB;
      const headroomDb = Math.max(3, SPEECH_TOP_DB - refDb);
      let rel = (this.fastDb - refDb) / headroomDb;
      if (db <= DIGITAL_SILENCE_DB) rel = 0;
      rel = Math.max(0, Math.min(rel, 1));

      // Perceptual sqrt curve compresses residual motion toward zero while
      // keeping quiet speech visible.
      normalized = Math.sqrt(rel);
    }

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
      this.fastDb = -75.0;
      this.slowDb = -75.0;
      this.primeBuf = [];
      this.gatePrimed = false;
      this.gateOpen = false;
      this.onsetStreak = 0;
      this.stuckStreak = 0;
    } else if (state === 'recording' || state === 'agent') {
      this.recordingStartTime = performance.now();
      this.recordingFramesReceived = 0;
      this.smoothedAmplitude = 0;
      this.lastAmplitudeAt = 0;
      this.fastDb = -75.0;
      this.slowDb = -75.0;
      this.primeBuf = [];
      this.gatePrimed = false;
      this.gateOpen = false;
      this.onsetStreak = 0;
      this.stuckStreak = 0;
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

    const speed = 0.48 + this.smoothedAmplitude * 0.82;
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

    const isBubble = this.overlayRoot?.classList.contains('style-bubble');
    const primaryColor = isAgent ? '#00F5D4' : '#B08AC8';
    const forefrontColor = isAgent ? '#E6FFFA' : '#F1EAF5';
    const primaryAlpha = 0.32;

    const centerY = H / 2;
    // Production transcription meter: 6% calm baseline, 40% height headroom,
    // softer parabolic envelope. Bubble tier is scaled down to keep the wave
    // inside the 44px orb without clipping the circular edge.
    const bubbleScale = isBubble ? 0.62 : 1;
    const activeAmplitude = (this.smoothedAmplitude * 0.88 + 0.06) * (H * 0.40) * bubbleScale;

    const phase1 = this.phase;
    const phase2 = -this.phase * 0.7;

    const env = (x) => Math.pow(Math.sin((x / W) * Math.PI), 1.22);

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

    // Micro-detail is gated: perfectly clean at silence, subtle at speech.
    const vib = this.smoothedAmplitude > 0.04 ? this.smoothedAmplitude : 0;
    const lwScale = isBubble ? 0.82 : 1;

    // Wave 1: background — thin, airy, low opacity
    ctx.beginPath();
    ctx.moveTo(0, centerY);
    for (let x = 0; x <= W; x += step) {
      const e = env(x);
      const angle = (x / W) * 2 * Math.PI * 1.5 + phase1;
      const vibration = Math.sin(x * 0.1 + phase1 * vibPhase1) * vib * 2.0;
      const y = centerY + (Math.sin(angle) * activeAmplitude * 0.48 + vibration) * e;
      ctx.lineTo(x, y);
    }
    const grad1 = ctx.createLinearGradient(0, 0, W, 0);
    grad1.addColorStop(0, 'transparent');
    grad1.addColorStop(0.5, hexAlpha(primaryColor, primaryAlpha));
    grad1.addColorStop(1, 'transparent');
    ctx.strokeStyle = grad1;
    ctx.lineWidth = 1.35 * lwScale;
    ctx.stroke();

    // Wave 2: mid — the body of the meter, slightly bolder
    ctx.beginPath();
    ctx.moveTo(0, centerY);
    for (let x = 0; x <= W; x += step) {
      const e = env(x);
      const angle = (x / W) * 2 * Math.PI * 2.5 + phase2;
      const vibration = Math.sin(x * 0.15 - phase2 * vibPhase2) * vib * 1.45;
      const y = centerY + (Math.sin(angle) * activeAmplitude * 0.68 + vibration) * e;
      ctx.lineTo(x, y);
    }
    const grad2 = ctx.createLinearGradient(0, 0, W, 0);
    grad2.addColorStop(0, 'transparent');
    grad2.addColorStop(0.5, hexAlpha(primaryColor, primaryAlpha * 1.08));
    grad2.addColorStop(1, 'transparent');
    ctx.strokeStyle = grad2;
    ctx.lineWidth = 1.65 * lwScale;
    ctx.stroke();

    // Wave 3: forefront — crisp ink line, soft glow only when loud
    const glow = this.smoothedAmplitude > 0.28 ? this.smoothedAmplitude * 7 : 0;
    if (glow > 0) {
      ctx.shadowColor = hexAlpha(forefrontColor, 0.22);
      ctx.shadowBlur = glow;
    }
    ctx.beginPath();
    ctx.moveTo(0, centerY);
    for (let x = 0; x <= W; x += step) {
      const e = env(x);
      const angle = (x / W) * 2 * Math.PI * 1.2 + (phase1 - phase2) * 0.5;
      const vibration = Math.sin(x * 0.08 + (phase1 - phase2) * 0.5 * vibPhase3) * vib * 2.4;
      const y = centerY + (Math.sin(angle) * activeAmplitude * 0.88 + vibration) * e;
      ctx.lineTo(x, y);
    }
    const grad3 = ctx.createLinearGradient(0, 0, W, 0);
    grad3.addColorStop(0, 'transparent');
    grad3.addColorStop(0.5, hexAlpha(forefrontColor, 0.90));
    grad3.addColorStop(1, 'transparent');
    ctx.strokeStyle = grad3;
    ctx.lineWidth = 1.90 * lwScale;
    ctx.stroke();
    ctx.shadowBlur = 0;
    ctx.shadowColor = 'transparent';
  }
}

function hexAlpha(hex, alpha) {
  const r = parseInt(hex.slice(1, 3), 16);
  const g = parseInt(hex.slice(3, 5), 16);
  const b = parseInt(hex.slice(5, 7), 16);
  return `rgba(${r},${g},${b},${alpha})`;
}

window.AuraVisualizer = AuraVisualizer;
