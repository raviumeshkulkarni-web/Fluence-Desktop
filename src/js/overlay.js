/**
 * Fluence Windows — Overlay Window JS
 * 
 * Controls the floating recording overlay states and connects to
 * Tauri IPC events from the Rust backend.
 * 
 * Design: Precision Ink status card — three luminance zones,
 * waveform hero, duration timer, mode badge.
 */

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

// DOM refs
const overlayRoot = document.getElementById('overlay-root');
const recLabel    = document.getElementById('rec-label');
const modeBadge   = document.getElementById('mode-badge');
const cardTimer   = document.getElementById('card-timer');
const cardDiscard = document.getElementById('card-discard');
const statusMsg   = document.getElementById('status-msg');
const statusRetry = document.getElementById('status-retry');
const recHint     = document.getElementById('rec-hint');

// State
let currentState = 'idle';
let aura = null;
let cachedSettings = null;
let timerInterval = null;
let recordingStartTime = 0;
let retryTimer = null;
let nextSessionId = 0;
let activeSessionId = 0;
let chimeCtx = null;

function beginSession() {
  activeSessionId = ++nextSessionId;
  return activeSessionId;
}

function isSessionActive(sessionId) {
  return sessionId !== 0 && sessionId === activeSessionId;
}

function completeSession(sessionId) {
  if (isSessionActive(sessionId)) activeSessionId = 0;
}

function resetTransientUi() {
  clearAutoDismiss();
  hideRetry();
  hideRecHint();
  setStatusMessage('');
  hideAppPill();
}

// ── Initialization ──────────────────────────────────────────────

window.addEventListener('DOMContentLoaded', async () => {
  aura = new AuraVisualizer('waveform-canvas');
  setupEventListeners();
  setupDiscardButton();
  setupRetryButton();
  // Reset synchronously before any awaits so a hotkey that fires while the
  // settings fetch is in flight never observes a stale state (which would
  // make applyAppInfo skip the pill for that session).
  setState('idle');
  try {
    const prefs = await getRecordingPreferences();
    applyOverlayStyle(prefs.overlayStyle);
  } catch {}
});

// ── Tauri Event Listeners ───────────────────────────────────────

async function setupEventListeners() {

  // Hotkey events from Rust (Transcription Mode)
  await listen('hotkey-start-recording', async () => {
    console.log('hotkey-start-recording event received');
    const sessionId = beginSession();
    resetTransientUi();
    setState('recording');
    setMode('stt');
    if (overlayRoot) overlayRoot.classList.remove('active');
    // Capture the foreground app while the overlay is still hidden so the
    // target app owns focus (timing-sensitive).
    const appIcon = loadAppIcon(sessionId);
    let recordingStarted = false;
    try {
      const prefs = await getRecordingPreferences();
      await invoke('start_recording', { deviceId: prefs.audioDeviceId });
      recordingStarted = true;
      if (!isSessionActive(sessionId)) {
        await invoke('stop_recording').catch(() => {});
        return;
      }
      setOverlayCorner(prefs.overlayPosition);
      applyOverlayStyle(prefs.overlayStyle);
      await invoke('show_overlay', { position: prefs.overlayPosition });
      if (!isSessionActive(sessionId)) {
        await invoke('stop_recording').catch(() => {});
        return;
      }
      updateRecHint();

      // Trigger opening transition immediately
      if (overlayRoot) overlayRoot.classList.add('active');
      startTimer();
      void appIcon;
    } catch (err) {
      console.error('Failed to start/show recording:', err);
      if (recordingStarted) {
        await invoke('stop_recording').catch(stopErr =>
          console.error('Failed to clean up recording start:', stopErr)
        );
      }
      if (isSessionActive(sessionId)) {
        setState('idle');
        completeSession(sessionId);
      }
    }
  });

  await listen('hotkey-stop-recording', async () => {
    const sessionId = activeSessionId;
    if (!isSessionActive(sessionId) || (currentState !== 'recording' && currentState !== 'agent')) return;
    stopTimer();
    await stopAndTranscribe(false, sessionId);
  });

  // Hotkey events from Rust (Agent Mode)
  await listen('hotkey-start-agent-recording', async () => {
    console.log('hotkey-start-agent-recording event received');
    const sessionId = beginSession();
    resetTransientUi();
    setState('agent');
    setMode('agent');
    if (overlayRoot) overlayRoot.classList.remove('active');
    // Capture the foreground app while the overlay is still hidden so the
    // target app owns focus (timing-sensitive).
    const appIcon = loadAppIcon(sessionId);
    let recordingStarted = false;
    try {
      const prefs = await getRecordingPreferences();
      await invoke('start_recording', { deviceId: prefs.audioDeviceId });
      recordingStarted = true;
      if (!isSessionActive(sessionId)) {
        await invoke('stop_recording').catch(() => {});
        return;
      }
      setOverlayCorner(prefs.overlayPosition);
      applyOverlayStyle(prefs.overlayStyle);
      await invoke('show_overlay', { position: prefs.overlayPosition });
      if (!isSessionActive(sessionId)) {
        await invoke('stop_recording').catch(() => {});
        return;
      }
      updateRecHint();

      // Trigger opening transition immediately
      if (overlayRoot) overlayRoot.classList.add('active');
      startTimer();
      void appIcon;
    } catch (err) {
      console.error('Failed to start/show recording (agent):', err);
      if (recordingStarted) {
        await invoke('stop_recording').catch(stopErr =>
          console.error('Failed to clean up agent recording start:', stopErr)
        );
      }
      if (isSessionActive(sessionId)) {
        setState('idle');
        completeSession(sessionId);
      }
    }
  });

  await listen('hotkey-stop-agent-recording', async () => {
    const sessionId = activeSessionId;
    if (!isSessionActive(sessionId) || (currentState !== 'recording' && currentState !== 'agent')) return;
    stopTimer();
    await stopAndTranscribe(true, sessionId);
  });

  // Live amplitude data from Rust audio stream
  await listen('audio-amplitude', (evt) => {
    let raw = evt.payload;
    if (typeof raw === 'object' && raw !== null) {
      raw = raw.payload ?? raw.value ?? 0;
    }
    if (aura && typeof raw === 'number') {
      aura.setAmplitude(raw);
    }
  });

  // Navigate overlay to specific state (from main window)
  await listen('overlay-state', (evt) => {
    setState(evt.payload);
  });
}

// ── Timer ───────────────────────────────────────────────────────

function startTimer() {
  stopTimer();
  recordingStartTime = Date.now();
  updateTimerDisplay();
  timerInterval = setInterval(updateTimerDisplay, 1000);
}

function stopTimer() {
  if (timerInterval) {
    clearInterval(timerInterval);
    timerInterval = null;
  }
}

function updateTimerDisplay() {
  const elapsed = Math.floor((Date.now() - recordingStartTime) / 1000);
  const mins = Math.floor(elapsed / 60);
  const secs = elapsed % 60;
  if (cardTimer) {
    cardTimer.textContent = `${mins.toString().padStart(2, '0')}:${secs.toString().padStart(2, '0')}`;
  }
}

// ── Mode Badge ──────────────────────────────────────────────────

function setMode(mode) {
  if (modeBadge) {
    modeBadge.textContent = mode === 'agent' ? 'Agent Mode' : 'Transcribe';
  }
}

// ── Foreground App Pill ──────────────────────────────────────────

let appPollTimer = null;
let appPollRequestId = 0;

async function loadAppIcon(sessionId) {
  const pill = document.getElementById('app-pill');
  const pillIcon = document.getElementById('app-pill-icon');
  const pillName = document.getElementById('app-pill-name');
  if (!pill || !pillIcon || !pillName) return;
  pill.hidden = true;
  try {
    const info = await invoke('get_foreground_app_icon');
    if (isSessionActive(sessionId) && info && info.name && info.icon_data_url) {
      applyAppInfo(info);
    }
  } catch (err) {
    console.warn('Failed to load foreground app icon:', err);
  }
}

function applyAppInfo(info) {
  const pill = document.getElementById('app-pill');
  const pillIcon = document.getElementById('app-pill-icon');
  const pillName = document.getElementById('app-pill-name');
  if (!pill || !pillIcon || !pillName || !info || !info.name) return;
  if (currentState !== 'recording' && currentState !== 'agent') return;
  if (pillName.textContent !== info.name) {
    pillIcon.src = info.icon_data_url || pillIcon.src;
    pillIcon.alt = `${info.name} icon`;
    pillName.textContent = info.name;
  }
  pill.hidden = false;
  // Apply synchronously — requestAnimationFrame can be dropped while the
  // overlay window is still hidden, which would leave the pill at opacity 0.
  pill.classList.add('visible');
}

// Poll the foreground app while recording so the pill tracks app switches.
function startAppPolling() {
  stopAppPolling();
  const sessionId = activeSessionId;
  appPollTimer = setInterval(async () => {
    if (!isSessionActive(sessionId) || (currentState !== 'recording' && currentState !== 'agent')) {
      stopAppPolling();
      return;
    }
    const requestId = ++appPollRequestId;
    try {
      const info = await invoke('get_foreground_app_icon');
      if (
        isSessionActive(sessionId) &&
        requestId === appPollRequestId &&
        info &&
        info.name
      ) {
        applyAppInfo(info);
      }
    } catch (err) {
      // Transient — keep showing the current pill.
    }
  }, 1200);
}

function stopAppPolling() {
  appPollRequestId += 1;
  if (appPollTimer) {
    clearInterval(appPollTimer);
    appPollTimer = null;
  }
}

function hideAppPill() {
  stopAppPolling();
  const pill = document.getElementById('app-pill');
  if (pill) {
    pill.classList.remove('visible');
    pill.hidden = true;
    const pillName = document.getElementById('app-pill-name');
    if (pillName) pillName.textContent = '';
  }
}

function setOverlayCorner(position) {
  if (!overlayRoot) return;
  overlayRoot.classList.remove('corner-bottom-left', 'corner-bottom-right', 'corner-bottom-center');
  if (position === 'bottom_left') {
    overlayRoot.classList.add('corner-bottom-left');
  } else if (position === 'bottom_right') {
    overlayRoot.classList.add('corner-bottom-right');
  } else {
    overlayRoot.classList.add('corner-bottom-center');
  }
}

function applyOverlayStyle(style) {
  if (!overlayRoot) return;
  overlayRoot.classList.remove('style-full', 'style-compact', 'style-bubble');
  const normalized = (style === 'compact' || style === 'bubble') ? style : 'full';
  overlayRoot.classList.add(`style-${normalized}`);
}

// ── Status Message + Retry ──────────────────────────────────────

function setStatusMessage(text) {
  if (statusMsg) statusMsg.textContent = text || '';
}

function showRetry() {
  if (statusRetry) statusRetry.hidden = false;
}

function hideRetry() {
  if (statusRetry) statusRetry.hidden = true;
}

function clearAutoDismiss() {
  if (retryTimer) {
    clearTimeout(retryTimer);
    retryTimer = null;
  }
}

function scheduleAutoDismiss(ms) {
  const sessionId = activeSessionId;
  clearAutoDismiss();
  retryTimer = setTimeout(() => {
    if (isSessionActive(sessionId)) void fadeAndHide(sessionId);
  }, ms);
}

function setupRetryButton() {
  if (statusRetry) {
    statusRetry.addEventListener('click', async () => {
      clearAutoDismiss();
      hideRetry();
      setStatusMessage('');
      const sessionId = activeSessionId;
      if (!isSessionActive(sessionId)) return;
      setState('transcribing');
      if (recLabel) recLabel.textContent = 'PROCESSING';
       await runSttFlow(sessionId, true);
    });
  }
}

// ── Hold-to-Record Hint ──────────────────────────────────────────

function updateRecHint() {
  if (!recHint) return;
  const isHoldToRecord =
    (currentState === 'recording' && cachedSettings?.recording_mode === 'hold_to_record') ||
    (currentState === 'agent' && cachedSettings?.agent_recording_mode === 'hold_to_record');
  recHint.textContent = isHoldToRecord ? 'RELEASE TO STOP' : '';
  recHint.hidden = !isHoldToRecord;
}

function hideRecHint() {
  if (recHint) recHint.hidden = true;
}

// ── Completion Chime ─────────────────────────────────────────────

function playCompletionChime() {
  try {
    if (cachedSettings && cachedSettings.sound_on_complete === false) return;
    const Ctx = window.AudioContext || window.webkitAudioContext;
    if (!Ctx) return;
    if (!chimeCtx) chimeCtx = new Ctx();
    if (chimeCtx.state === 'suspended') {
      chimeCtx.resume().catch(() => {});
    }
    if (chimeCtx.state !== 'running') return;
    const t0 = chimeCtx.currentTime;
    const gain = chimeCtx.createGain();
    gain.gain.setValueAtTime(0.0001, t0);
    gain.gain.exponentialRampToValueAtTime(0.12, t0 + 0.01);
    gain.gain.exponentialRampToValueAtTime(0.0001, t0 + 0.35);
    gain.connect(chimeCtx.destination);
    [523.25, 659.25].forEach((freq, i) => {
      const osc = chimeCtx.createOscillator();
      osc.type = 'sine';
      osc.frequency.setValueAtTime(freq, t0 + i * 0.11);
      osc.connect(gain);
      osc.start(t0 + i * 0.11);
      osc.stop(t0 + i * 0.11 + 0.3);
    });
  } catch (err) {
    console.warn('Completion chime failed:', err);
  }
}

// ── Recording Flow ──────────────────────────────────────────────

function setupDiscardButton() {
  if (cardDiscard) {
    cardDiscard.addEventListener('click', async (e) => {
      e.stopPropagation();
      stopTimer();
      const sessionId = beginSession();
      resetTransientUi();
      try {
        await invoke('stop_recording');
      } catch (err) {
        console.error('Stop recording failed:', err);
      }
      await fadeAndHide(sessionId);
    });
  }
}

async function getRecordingPreferences() {
  try {
    cachedSettings = await invoke('get_settings');
  } catch {
    cachedSettings = cachedSettings || {};
  }

  return {
    overlayPosition: cachedSettings.overlay_position || 'bottom_right',
    overlayStyle: cachedSettings.overlay_style || 'full',
    audioDeviceId: cachedSettings.audio_device_id || null,
  };
}

async function fadeAndHide(sessionId = activeSessionId) {
  if (!isSessionActive(sessionId)) return;
  stopTimer();
  clearAutoDismiss();
  hideRetry();
  setStatusMessage('');
  hideAppPill();
  if (overlayRoot) {
    overlayRoot.classList.remove('active');
  }
  await new Promise(r => setTimeout(r, 200)); // let CSS exit transition finish
  if (!isSessionActive(sessionId)) return;
  await invoke('hide_overlay');
  if (!isSessionActive(sessionId)) return;
  setState('idle');
  completeSession(sessionId);
}

async function stopAndTranscribe(agentMode, sessionId) {
  if (!isSessionActive(sessionId)) return;
  setState(agentMode ? 'agent_transcribing' : 'transcribing');

  // Update label for transcribing state
  if (recLabel) recLabel.textContent = 'PROCESSING';

  let startTs = Date.now();

  try {
    if (agentMode) {
      console.time('StopAndTranscribeAgent');
      const selectionPromise = invoke('grab_active_selection').catch((err) => {
        console.warn('Failed to grab active selection early:', err);
        return null;
      });

      const [result, settings] = await Promise.all([
        invoke('stop_and_transcribe_recording'),
        invoke('get_settings'),
      ]);
      if (!result.text || !result.text.trim() || !/[\p{L}\p{N}]/u.test(result.text || '')) {
        await fadeAndHide(sessionId);
        return;
      }

      const selection = await selectionPromise;
      if (isSessionActive(sessionId)) {
        await handleAgentMode(result.text, settings, result.durationMs || (Date.now() - startTs), selection, sessionId);
      }
    } else {
      await runSttFlow(sessionId);
    }
  } catch (err) {
    console.error('Transcription error:', err);
    if (!isSessionActive(sessionId)) return;
    setState('error');
    if (agentMode) {
      setStatusMessage('Failed');
      scheduleAutoDismiss(2000);
    } else {
      setStatusMessage('Transcription failed');
      showRetry();
      scheduleAutoDismiss(4000);
    }
  }
}

async function runSttFlow(sessionId, retry = false) {
  if (!isSessionActive(sessionId)) return;
  const startTs = Date.now();
  try {
    const result = await invoke(retry ? 'retry_transcription_flow' : 'finish_transcription_flow');

    const hasAlphanumeric = /[\p{L}\p{N}]/u.test(result.text || '');
    if (!result.text || !result.text.trim() || !hasAlphanumeric) {
      await fadeAndHide(sessionId);
      return;
    }

    if (!isSessionActive(sessionId)) return;

    invoke('save_history_entry', {
      text: result.text,
      mode: 'transcription',
      durationMs: result.durationMs || (Date.now() - startTs),
      provider: result.provider,
    }).catch(err => console.error('Failed to save history entry:', err));

    try {
      await invoke('inject_text', { text: result.text, monitorAutoLearn: true });
      if (!isSessionActive(sessionId)) return;
      setState('success');
      setStatusMessage('Inserted');
      playCompletionChime();
      await new Promise(r => setTimeout(r, 1000));
      await fadeAndHide(sessionId);
    } catch (err) {
      console.error('Inject failed:', err);
      if (!isSessionActive(sessionId)) return;
      setState('error');
      setStatusMessage('Insert failed');
      scheduleAutoDismiss(4000);
    }
  } catch (err) {
    console.error('Transcription error:', err);
    if (!isSessionActive(sessionId)) return;
    setState('error');
    setStatusMessage('Transcription failed');
    showRetry();
    scheduleAutoDismiss(4000);
  }
}

async function handleAgentMode(voiceCommand, settings, durationMs, preGrabbedSelection, sessionId) {
  if (!isSessionActive(sessionId)) return;
  try {
    const llmPreset = settings.llm_provider.preset || 'groq';
    const llmTarget = `Fluence/LLM_ApiKey/${llmPreset.toLowerCase().replace(/ /g, '_')}`;
    const llmKey = await invoke('get_api_key', {
      target: llmTarget
    }).catch(() => '');
    if (!isSessionActive(sessionId)) return;

    let clipboardCtx = '';
    let grabbed = false;
    if (settings.auto_grab_highlight !== false) {
      if (preGrabbedSelection && preGrabbedSelection.trim()) {
        clipboardCtx = preGrabbedSelection;
        grabbed = true;
        console.log('Successfully grabbed text selection as context:', preGrabbedSelection);
      }
    }

    if (!grabbed) {
      try {
        clipboardCtx = (await invoke('grab_active_selection').catch(() => '')) || '';
      } catch {
        // Clipboard read may fail if no permission — proceed without context
      }
    }

    const action = await invoke('execute_agent_command', {
      req: {
        base_url: settings.llm_provider.base_url,
        api_key: llmKey || '',
        model: settings.llm_provider.model,
        voice_command: voiceCommand,
        clipboard_context: clipboardCtx,
      }
    });

    if (!isSessionActive(sessionId)) return;

    if (action.action === 'copy') {
      try {
        await invoke('copy_text', { text: action.content || '' });
      } catch (err) {
        console.error('Failed to copy to clipboard:', err);
      }
      if (!isSessionActive(sessionId)) return;
      setState('success');
      setStatusMessage('Copied');
      playCompletionChime();
      await new Promise(r => setTimeout(r, 900));
      await fadeAndHide(sessionId);
    } else {
      let executed = false;
      if (action.action === 'insert' || action.action === 'rewrite') {
        const textToInsert = action.content || '';
        await invoke('inject_text', { text: textToInsert, monitorAutoLearn: false });
        executed = true;
      } else if (action.action === 'delete_chars') {
        await invoke('execute_keyboard_action', {
          action: 'delete_chars',
          charCount: action.char_count || 0,
        });
        executed = true;
      } else if (action.action === 'select_all') {
        await invoke('execute_keyboard_action', { action: 'select_all', charCount: null });
        executed = true;
      } else if (action.action === 'submit') {
        await invoke('execute_keyboard_action', { action: 'submit', charCount: null });
        executed = true;
      }

      if (!isSessionActive(sessionId)) return;

      setState('success');
      setStatusMessage(executed ? 'Executed' : 'Done');
      playCompletionChime();
      await new Promise(r => setTimeout(r, 800));
      await fadeAndHide(sessionId);
    }

    invoke('save_history_entry', {
      text: `[Agent] ${voiceCommand}`,
      mode: 'agent',
      durationMs,
      provider: settings.llm_provider.preset,
    }).catch(err => console.error('Failed to save history entry:', err));

  } catch (err) {
    console.error('Agent Error:', err);
    if (!isSessionActive(sessionId)) return;
    setState('error');
    setStatusMessage('Failed');
    scheduleAutoDismiss(2000);
  }
}

// ── State Management ────────────────────────────────────────────

function setState(state) {
  currentState = state;

  if (aura) aura.setState(state);

  // Track the foreground app while actively recording so the app pill
  // updates if the user switches to another window mid-recording.
  if (state === 'recording' || state === 'agent') {
    startAppPolling();
  } else {
    stopAppPolling();
  }

  if (overlayRoot) {
    if (state !== 'idle') {
      overlayRoot.classList.add('active');
    } else {
      overlayRoot.classList.remove('active');
    }
  }

  // Update rec label based on state
  if (recLabel) {
    switch (state) {
      case 'recording':
      case 'agent':
        recLabel.textContent = 'LISTENING';
        break;
      case 'transcribing':
      case 'agent_transcribing':
        recLabel.textContent = 'PROCESSING';
        break;
      default:
        recLabel.textContent = 'LISTENING';
    }
  }

  updateRecHint();
}
