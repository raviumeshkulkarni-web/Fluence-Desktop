/**
 * Fluence Windows - Canvas Waveform Visualizer
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

function smoothstep(edge0, edge1, x) {
  if (edge0 === edge1) return x < edge0 ? 0 : 1;
  const t = Math.max(0, Math.min(1, (x - edge0) / (edge1 - edge0)));
  return t * t * (3 - 2 * t);
}

class AuraVisualizer {
  constructor(canvasId) {
    this.canvas = document.getElementById(canvasId);
    this.ctx = this.canvas?.getContext('2d');
    this.overlayRoot = document.getElementById('overlay-root');
    this.currentState = 'idle';
    this.style = 'waves';            // "waves" | "bars"
    this.targetAmplitude = 0;        // display target from ~30Hz telemetry
    this.smoothedAmplitude = 0;      // integrated smoothly at the rAF rate
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
    this.silenceStreak = 0;
    this.quietStreak = 0;
    this.hist = [];              // fastDb window for envelope-modulation spread
    this.spread = 0;             // dB spread across hist (speech ~modulated)
    this._barBg = [];
    this._barFt = [];
    this._gradCache = new Map();
    this._gradCacheW = 0;
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
    this._gradCache?.clear();
    this._gradCacheW = cssW;
  }

  _getWaveGrad(ctx, W, hex, alpha) {
    if (this._gradCacheW !== W) {
      this._gradCache.clear();
      this._gradCacheW = W;
    }
    const key = `w:${hex}:${alpha.toFixed(3)}`;
    let grad = this._gradCache.get(key);
    if (!grad) {
      grad = ctx.createLinearGradient(0, 0, W, 0);
      grad.addColorStop(0, 'transparent');
      grad.addColorStop(0.5, hexAlpha(hex, alpha));
      grad.addColorStop(1, 'transparent');
      this._gradCache.set(key, grad);
    }
    return grad;
  }

  _getBarGrad(ctx, W, hex, alpha) {
    if (this._gradCacheW !== W) {
      this._gradCache.clear();
      this._gradCacheW = W;
    }
    const key = `b:${hex}:${alpha.toFixed(3)}`;
    let grad = this._gradCache.get(key);
    if (!grad) {
      grad = ctx.createLinearGradient(0, 0, W, 0);
      grad.addColorStop(0, hexAlpha(hex, alpha * 0.22));
      grad.addColorStop(0.5, hexAlpha(hex, alpha));
      grad.addColorStop(1, hexAlpha(hex, alpha * 0.22));
      this._gradCache.set(key, grad);
    }
    return grad;
  }

  setAmplitude(rawAmplitude) {
    if (this.currentState !== 'recording' && this.currentState !== 'agent') {
      return;
    }

    this.recordingFramesReceived++;
    this.lastAmplitudeAt = performance.now();

    // Safety sanitize input in [0.0, 1.0]
    const raw = Math.max(0, Math.min(Number(rawAmplitude) || 0, 1.0));

    // Noise gate + relative meter (frontend-only; the Rust audio pipeline is
    // frozen).
    //
    // The backend maps RMS onto an absolute dBFS-linear value in [0,1]
    // spanning -75…-30 dBFS, so the same physical input can land anywhere
    // from ~0.05 (clean laptop mic) to ~0.85 (boosted / AGC input, Windows
    // Mic Boost) purely because of the device's capture gain. Any absolute
    // threshold therefore fails on some machines, and the raw EMIT itself
    // moves when the user changes the Windows input volume.
    //
    // Instead of gating against absolute levels, this inverts the map back to
    // true dBFS and runs a differential meter whose reference is a
    // continuously RE-ANCHORING noise floor - the broadcast-PPM approach,
    // normalized the way professional mixers meter (relative to a learned
    // floor, never absolute dBFS), so the absolute capture gain cancels out:
    //   * signal near the floor (ambience)     → the floor re-anchors upward,
    //     so raising the mic volume or an AGC gain pump becomes the NEW
    //     reference instead of false "speech"
    //   * signal BELOW the floor (quieting)    → the floor follows it down,
    //     so it never rides high above true silence
    //   * signal far above the floor (speech)  → the floor is frozen, so an
    //     utterance can never inflate the reference
    // Every decision is a dB DELTA from the floor, so the meter behaves
    // identically at any input volume, on any device, no per-machine tuning.
    const db = raw * 45 - 75;
    const DIGITAL_SILENCE_DB = -72; // muted mic / empty-buffer emits
    const ONSET_DB = 11;            // level onset above floor = speech key
    const CLOSE_DB = 4;             // fall back within this of floor = rest
    const FLOOR_FOLLOW_DOWN = 0.10; // floor follow rate when level falls below it
    const SILENCE_HOLD_COUNT = 5;   // consecutive dead-buffer frames to latch the gate shut
    // Modulation gate (why the plain ONSET_DB gate goes deaf at t=0): when a
    // recording starts straight into continuous speech the prime window seeds
    // the floor AT speech level, so a plain level-relative onset can never
    // fire and the floor even creeps up onto the utterance - the meter stays
    // flat until the FIRST pause re-anchors it and re-speech triggers, which
    // is exactly the reported 5-10 s dead waveform. Speech is not just
    // louder than the floor, it MODULATES rhythmically (~syllabic envelope),
    // while raised/steady ambience and mic-volume raises barely move. So the
    // open key is level-onset OR envelope spread; both measured in dB deltas
    // from the floor so gain cancels out.
    const MOD_HIST = 15;        // ~0.5 s fastDb window for spread
    const MOD_OPEN = 4.5;       // spread ⇒ speech-like modulation
    const MOD_CLOSE = 3.0;      // spread below this ⇒ steady/quiet
    const OPEN_STREAK = 2;      // speech-key frames to open the gate (~70 ms)
    const CLOSE_STREAK = 4;     // quiet+returned-frames to latch shut (~130 ms)
    const SPEECH_SPAN_DB = 28;  // floor→full-meter span for display
    const FLOOR_UP_FAST = 0.045;// floor re-anchor rate during steady stretches

    // Priming (~0.8 s of live telemetry, digital zeros excluded): seed the
    // floor BELOW the lowest observed live level (-55 dBFS floor) so speaking
    // through the first second or a hotkey chime can never pin the reference
    // onto the utterance (the committed lower-quartile seed does exactly that
    // when a recording starts straight into continuous speech, and the meter
    // stays flat until the user's first pause). A loud-but-steady ambience
    // still re-anchors afterwards because spread stays negligible and the
    // floor follows the steady level up. Until primed the gate stays closed;
    // only the display mirrors the live level (cosmetically compressed), per
    // the else branch below.
    if (!this.gatePrimed) {
      if (db > DIGITAL_SILENCE_DB) this.primeBuf.push(db);
      if (this.primeBuf.length >= 24) {
        this.primeBuf.sort((a, b) => a - b);
        this.slowDb = Math.min(this.primeBuf[0], -55);
        this.fastDb = this.slowDb;
        this.gatePrimed = true;
      } else {
        // Display-only reactivity while the noise floor is being learned.
        // Gate logic (gateOpen, slowDb, fastDb, primeBuf) is completely
        // untouched - the reference still seeds identically. The paint
        // mirrors the live level through a fixed compression (half gain,
        // capped below the gated full scale, with the same calm floor) so
        // the waveform tracks the user's voice from the first frame instead
        // of idling flat for the ~0.8 s prime window. The animation loop applies
        // attack/decay smoothing at display frame rate. A hotkey chime may
        // briefly flash during this window; the real gate still ignores it,
        // so the meter reference is never misled.
        this.targetAmplitude = Math.min(0.6, raw * 0.5);
      }
      return;
    }

    // Fast envelope (~45 ms). The floor tracker now runs on EVERY live frame -
    // open or closed - so ambient / gain / AGC changes re-anchor the
    // reference within ~1-2 s instead of latching the meter open for the rest
    // of the session. (The previous tracker only learned while the gate was
    // closed and froze while it was open; a mid-session mic-volume raise then
    // left the level permanently ≥ CLOSE_DB above the floor, which is exactly
    // why the waveform kept reacting to silence and tracked the user's input
    // volume.) Digital silence (mic muted, empty-buffer dropout) carries no
    // ambience information; it merely latches the gate shut.
    this.fastDb += (db - this.fastDb) * 0.5;
    const live = db > DIGITAL_SILENCE_DB;

    // Envelope-modulation spread over the recent fastDb window. Speech has a
    // pronounced syllabic envelope (spread well past MOD_OPEN on ~0.5 s), while
    // steady ambience / mic-volume changes barely move the envelope.
    this.hist.push(this.fastDb);
    if (this.hist.length > MOD_HIST) this.hist.shift();
    let spread = 0;
    if (this.hist.length >= MOD_HIST) {
      let mn = Infinity, mx = -Infinity;
      for (const v of this.hist) { if (v < mn) mn = v; if (v > mx) mx = v; }
      spread = mx - mn;
    }
    this.spread = spread;

    if (live) {
      const aboveFloor = this.fastDb - this.slowDb;
      // Floor re-anchors down fast when level falls below it, and follows a
      // RISING level up only while the envelope is steady (< MOD_CLOSE) - a
      // loud-but-calm ambience, a mic-volume raise, or an AGC gain pump
      // becomes the new reference instead of false "speech". During speech the
      // envelope modulates so the floor freezes, letting the rise show as
      // signal instead of inflating the reference onto the utterance (which is
      // what made the pre-fix meter track the user's input volume and stay
      // flat on continuous speech).
      if (aboveFloor < 0) {
        this.slowDb += aboveFloor * FLOOR_FOLLOW_DOWN;
      } else if (spread < MOD_CLOSE) {
        this.slowDb += aboveFloor * FLOOR_UP_FAST;
      }
      if (this.slowDb < -75) this.slowDb = -75;
      this.silenceStreak = 0;
    } else {
      this.silenceStreak++;
      if (this.silenceStreak >= SILENCE_HOLD_COUNT) {
        this.gateOpen = false;
        this.onsetStreak = 0;
        this.quietStreak = 0;
      }
    }

    // Hysteresis: open on a level rise near/above ONSET_DB above the floor OR on
    // speech-like envelope modulation (spread ≥ MOD_OPEN) - either sustained
    // for OPEN_STREAK (~70 ms). The modulation key is what unwedges a
    // recording that starts straight into continuous speech, where the level
    // rise over the seeded floor alone stays just shy of ONSET_DB. Close when
    // the level returns near the floor AND the envelope goes steady for
    // CLOSE_STREAK (~130 ms): that is real rest, not a pause in speech.
    const diff = this.fastDb - this.slowDb;
    const speechKey = live && (diff >= ONSET_DB || (spread >= MOD_OPEN && diff > 0));
    if (speechKey) this.onsetStreak++;
    else this.onsetStreak = 0;
    if (this.onsetStreak >= OPEN_STREAK) this.gateOpen = true;
    if (this.gateOpen) {
      if (live && diff < CLOSE_DB && spread < MOD_CLOSE) this.quietStreak++;
      else this.quietStreak = 0;
      if (this.quietStreak >= CLOSE_STREAK) {
        this.gateOpen = false;
        this.onsetStreak = 0;
        this.quietStreak = 0;
      }
    }

    let normalized = 0;
    if (this.gateOpen && live) {
      // Level relative to the re-anchored floor with a 3 dB knee, scaled over
      // a fixed floor→full-scale span, then perceptual sqrt.
      let rel = (this.fastDb - this.slowDb - 3) / SPEECH_SPAN_DB;
      rel = Math.max(0, Math.min(rel, 1));
      normalized = Math.sqrt(rel);
    }

    this.targetAmplitude = Math.max(0, Math.min(normalized, 1.0));
  }

  setState(state) {
    if (this.currentState === state) return;
    this.currentState = state;

    if (!this.overlayRoot) return;
    // Only manage the state-* classes. Wiping className here would destroy
    // the tier style (style-full/compact/bubble) and corner docking classes
    // applied by overlay.js on every state transition - the overlay would
    // snap back to the full-size card and the waveform canvas would jump.
    this.overlayRoot.classList.remove(
      'state-idle',
      'state-recording',
      'state-agent',
      'state-transcribing',
      'state-agent_transcribing',
      'state-success',
      'state-error',
      'state-no-speech'
    );
    if (state !== 'idle') this.overlayRoot.classList.add(`state-${state}`);

    if (state === 'idle' || state === 'transcribing' || state === 'agent_transcribing') {
      this.targetAmplitude = 0;
      if (state === 'idle') {
        this.smoothedAmplitude = 0;
        this._barBg = [];
        this._barFt = [];
      }
      this.recordingStartTime = 0;
      this.recordingFramesReceived = 0;
      this.lastAmplitudeAt = 0;
      this.fastDb = -75.0;
      this.slowDb = -75.0;
      this.primeBuf = [];
      this.gatePrimed = false;
      this.gateOpen = false;
      this.onsetStreak = 0;
      this.silenceStreak = 0;
      this.quietStreak = 0;
      this.hist = [];
      this.spread = 0;
    } else if (state === 'recording' || state === 'agent') {
      this.recordingStartTime = performance.now();
      this.recordingFramesReceived = 0;
      this.targetAmplitude = 0;
      this.smoothedAmplitude = 0;
      this._barBg = [];
      this._barFt = [];
      this.lastAmplitudeAt = 0;
      this.fastDb = -75.0;
      this.slowDb = -75.0;
      this.primeBuf = [];
      this.gatePrimed = false;
      this.gateOpen = false;
      this.onsetStreak = 0;
      this.silenceStreak = 0;
      this.quietStreak = 0;
      this.hist = [];
      this.spread = 0;
      if (this._rafId === null) {
        this._isRunning = true;
        this._loop(performance.now());
      }
    }
  }

  getState() { return this.currentState; }

  // Waveform animation pick (Floating Bubble > Appearance). Unknown or
  // missing values normalize back to "waves" so a hand-edited settings
  // file can never break the draw loop. Switching styles keeps the
  // running meter (amplitude, gate, phase) untouched, so a mid-recording
  // change glides instead of snapping.
  setStyle(style) {
    this.style = (style === 'bars') ? 'bars' : 'waves';
  }

  _loop(timestamp) {
    if (!this._isRunning && this.currentState === 'idle') {
      this._rafId = null;
      return;
    }

    this._rafId = requestAnimationFrame((t) => this._loop(t));

    if (this.lastTime === null) { this.lastTime = timestamp; }
    const dt = Math.min(Math.max((timestamp - this.lastTime) / 1000, 0), 0.1);
    this.lastTime = timestamp;

    if (this.currentState === 'idle') {
      const heartbeatPeriod = 4000;
      const heartbeatPhase = (timestamp % heartbeatPeriod) / heartbeatPeriod;
      this.smoothedAmplitude = Math.sin(heartbeatPhase * Math.PI) * 0.12;
      this.targetAmplitude = this.smoothedAmplitude;
    } else if (this.currentState === 'recording' || this.currentState === 'agent') {
      // If telemetry stalls, drop the target smoothly instead of latching a
      // stale amplitude indefinitely.
      if (this.lastAmplitudeAt > 0 && timestamp - this.lastAmplitudeAt > 350) {
        this.targetAmplitude = 0;
      }
      const rate = this.targetAmplitude > this.smoothedAmplitude ? 18.0 : 5.5;
      const alpha = 1 - Math.exp(-rate * dt);
      this.smoothedAmplitude += (this.targetAmplitude - this.smoothedAmplitude) * alpha;
      if (this.smoothedAmplitude < 0.001 && this.targetAmplitude === 0) {
        this.smoothedAmplitude = 0;
      }
    } else if (this.currentState === 'transcribing' || this.currentState === 'agent_transcribing') {
      const alpha = 1 - Math.exp(-7.0 * dt);
      this.smoothedAmplitude += (0 - this.smoothedAmplitude) * alpha;
      if (this.smoothedAmplitude < 0.001) this.smoothedAmplitude = 0;
    }

    const speed = 0.48 + this.smoothedAmplitude * 0.82;
    this.phase = (this.phase + speed * dt * 2 * Math.PI) % (1000 * Math.PI);
    this._lastFrameAt = timestamp;

    try {
      this._draw(dt);
    } catch (err) {
      // A canvas/gradient exception must never kill the animation loop.
      console.warn('Waveform draw error:', err);
    }
  }

  _traceSmoothWave(ctx, W, step, sampleY) {
    ctx.beginPath();
    let prevX = 0;
    let prevY = sampleY(0);
    ctx.moveTo(prevX, prevY);
    for (let x = step; x < W; x += step) {
      const y = sampleY(x);
      const midX = (prevX + x) * 0.5;
      const midY = (prevY + y) * 0.5;
      ctx.quadraticCurveTo(prevX, prevY, midX, midY);
      prevX = x;
      prevY = y;
    }
    ctx.quadraticCurveTo(prevX, prevY, W, sampleY(W));
  }

  _draw(dt) {
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

    // Additive animation dispatch: "waves" keeps the exact original body
    // below. Bars draws on the same smoothed meter, phase, colors, and
    // per-tier scales, so the visualizer only changes paint, never
    // sensing.
    if (this.style === 'bars') {
      this._drawBars(W, H, dt);
      return;
    }

    ctx.lineCap = 'round';
    ctx.lineJoin = 'round';

    const s = this._styleFactors(W, H);
    const centerY = H / 2;
    const phase1 = this.phase;
    const phase2 = -this.phase * 0.7;
    const env = (x) => Math.pow(Math.sin((x / W) * Math.PI), 1.22);
    const step = W < 60 ? 1 : 2;
    const vibPhase1 = (0.1 * W) / (2 * Math.PI * 1.5);
    const vibPhase2 = (0.15 * W) / (2 * Math.PI * 2.5);
    const vibPhase3 = (0.08 * W) / (2 * Math.PI * 1.2);
    const vib = smoothstep(0.03, 0.14, s.level) * s.level;

    this._traceSmoothWave(ctx, W, step, (x) => {
      const e = env(x);
      const angle = (x / W) * 2 * Math.PI * 1.5 + phase1;
      const vibration = Math.sin(x * 0.1 + phase1 * vibPhase1) * vib * 2.0;
      return centerY + (Math.sin(angle) * s.base * 0.48 + vibration) * e;
    });
    ctx.strokeStyle = this._getWaveGrad(ctx, W, s.primaryColor, s.primaryAlpha);
    ctx.lineWidth = 1.35 * s.lwScale;
    ctx.stroke();

    this._traceSmoothWave(ctx, W, step, (x) => {
      const e = env(x);
      const angle = (x / W) * 2 * Math.PI * 2.5 + phase2;
      const vibration = Math.sin(x * 0.15 - phase2 * vibPhase2) * vib * 1.45;
      return centerY + (Math.sin(angle) * s.base * 0.68 + vibration) * e;
    });
    ctx.strokeStyle = this._getWaveGrad(ctx, W, s.primaryColor, s.primaryAlpha * 1.08);
    ctx.lineWidth = 1.65 * s.lwScale;
    ctx.stroke();

    const phase3 = (phase1 - phase2) * 0.5;
    this._traceSmoothWave(ctx, W, step, (x) => {
      const e = env(x);
      const angle = (x / W) * 2 * Math.PI * 1.2 + phase3;
      const vibration = Math.sin(x * 0.08 + phase3 * vibPhase3) * vib * 2.4;
      return centerY + (Math.sin(angle) * s.base * 0.88 + vibration) * e;
    });
    if (s.glowStrength > 0.01) {
      ctx.strokeStyle = this._getWaveGrad(ctx, W, s.forefrontColor, 0.18 * s.glowStrength);
      ctx.lineWidth = (4.6 + s.glowStrength * 1.8) * s.lwScale;
      ctx.stroke();
    }
    ctx.strokeStyle = this._getWaveGrad(ctx, W, s.forefrontColor, 0.90);
    ctx.lineWidth = 1.90 * s.lwScale;
    ctx.stroke();
  }

  // ── Bars (additive style) ─────────────────────────────────────────
  // Reads the exact production meter ("waves" factors): same mode colors
  // (amethyst / cyan), same tier line-width and bubble scales, same 6%
  // calm baseline and 40% headroom, same gated smoothedAmplitude, same
  // phase integration. Only the paint differs, so switching the setting
  // never changes sensing behavior.

  _styleFactors(W, H) {
    const isAgent = this.currentState === 'agent' || this.currentState === 'agent_transcribing';
    const isBubble = this.overlayRoot?.classList.contains('style-bubble');
    const isCompact = this.overlayRoot?.classList.contains('style-compact');
    const primaryColor = isAgent ? '#0BD6E3' : '#8B45D8';
    const forefrontColor = isAgent ? '#E6FFFA' : '#F1EAF5';
    const primaryAlpha = isCompact ? 0.26 : 0.32;
    const lwScale = isBubble ? 0.82 : isCompact ? 0.9 : 1;
    const bubbleScale = isBubble ? 0.62 : 1;
    const level = this.smoothedAmplitude;
    // Identical production meter: 6% calm baseline, 40% height headroom.
    const base = (level * 0.88 + 0.06) * (H * 0.40) * bubbleScale;
    const glowStrength = smoothstep(0.20, 0.72, level);
    return { primaryColor, forefrontColor, primaryAlpha, lwScale, bubbleScale, level, base, glowStrength };
  }

  _drawBars(W, H, dt) {
    const ctx = this.ctx;
    if (!ctx) return;
    ctx.lineCap = 'round';
    ctx.lineJoin = 'round';
    const s = this._styleFactors(W, H);
    const centerY = H / 2;
    // Pitch stays absolute pixels so tier width changes density, not shape.
    const count = W < 60 ? 7 : Math.min(25, Math.max(9, Math.floor(W / 6)));
    const edgeFloor = 0.22;
    const edge = (x) => { const s = Math.pow(Math.sin((x / W) * Math.PI), 0.7); return edgeFloor + (1 - edgeFloor) * s; };
    const clampH = (h) => Math.min(h, H * 0.48);

    if (this._barBg.length !== count) {
      this._barBg = new Array(count).fill(0);
      this._barFt = new Array(count).fill(0);
    }

    const attackAlpha = 1 - Math.exp(-22.0 * dt);
    const decayAlpha = 1 - Math.exp(-7.5 * dt);
    const freq1 = W < 60 ? 0.16 : 0.11;
    const freq2 = W < 60 ? 0.24 : 0.17;

    // One batched path for the softer background bed.
    ctx.beginPath();
    for (let i = 0; i < count; i++) {
      const x = ((i + 0.5) / count) * W;
      const e = edge(x);
      const u1 = Math.abs(Math.sin(x * freq1 + this.phase * 1.35));
      const u2 = Math.abs(Math.sin(x * freq2 - this.phase * 1.05));
      const shape = Math.min(1, u1 * 0.62 + u2 * 0.38);
      const targetBg = clampH((s.base * (0.34 + 0.52 * shape) + 1.2) * e);
      const prevBg = this._barBg[i];
      const hBg = prevBg + (targetBg - prevBg) * (targetBg > prevBg ? attackAlpha : decayAlpha);
      this._barBg[i] = hBg;
      ctx.moveTo(x, centerY - hBg);
      ctx.lineTo(x, centerY + hBg);
    }
    ctx.strokeStyle = this._getBarGrad(ctx, W, s.primaryColor, s.primaryAlpha * 1.05);
    ctx.lineWidth = 2.4 * s.lwScale;
    ctx.stroke();

    // One batched forefront path is stroked once for its soft bloom and once
    // for its crisp center line.
    ctx.beginPath();
    for (let i = 0; i < count; i++) {
      const x = ((i + 0.5) / count) * W;
      const e = edge(x);
      const u1 = Math.abs(Math.sin(x * freq1 + this.phase * 1.35));
      const u2 = Math.abs(Math.sin(x * freq2 - this.phase * 1.05));
      const shape = Math.min(1, u1 * 0.62 + u2 * 0.38);
      const targetFt = clampH((s.base * (0.10 + 0.62 * shape * Math.max(0.25, s.level)) + 1.6) * e);
      const prevFt = this._barFt[i];
      const hFt = prevFt + (targetFt - prevFt) * (targetFt > prevFt ? attackAlpha : decayAlpha);
      this._barFt[i] = hFt;
      ctx.moveTo(x, centerY - hFt);
      ctx.lineTo(x, centerY + hFt);
    }
    if (s.glowStrength > 0.01) {
      ctx.strokeStyle = this._getBarGrad(ctx, W, s.forefrontColor, 0.16 * s.glowStrength);
      ctx.lineWidth = (4.2 + s.glowStrength * 1.4) * s.lwScale;
      ctx.stroke();
    }
    ctx.strokeStyle = this._getBarGrad(ctx, W, s.forefrontColor, 0.88);
    ctx.lineWidth = 1.9 * s.lwScale;
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
