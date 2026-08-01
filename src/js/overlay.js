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

// State
let currentState = 'idle';
let aura = null;
let cachedSettings = null;
let timerInterval = null;
let recordingStartTime = 0;
let retryTimer = null;

// ── Initialization ──────────────────────────────────────────────

window.addEventListener('DOMContentLoaded', () => {
  aura = new AuraVisualizer('waveform-canvas');
  setupEventListeners();
  setupDiscardButton();
  setupRetryButton();
  setState('idle');
});

// ── Tauri Event Listeners ───────────────────────────────────────

async function setupEventListeners() {

  // Hotkey events from Rust (Transcription Mode)
  await listen('hotkey-start-recording', async () => {
    console.log('hotkey-start-recording event received');
    setState('recording');
    setMode('stt');
    if (overlayRoot) overlayRoot.classList.remove('active');
    // Capture the foreground app while the overlay is still hidden so the
    // target app owns focus (timing-sensitive).
    const appIcon = loadAppIcon();
    try {
      const prefs = await getRecordingPreferences();
      await invoke('start_recording', { deviceId: prefs.audioDeviceId });
      await appIcon;
      setOverlayCorner(prefs.overlayPosition);
      await invoke('show_overlay', { position: prefs.overlayPosition });

      // Trigger opening transition immediately
      if (overlayRoot) overlayRoot.classList.add('active');
      startTimer();
    } catch (err) {
      console.error('Failed to start/show recording:', err);
      setState('idle');
    }
  });

  await listen('hotkey-stop-recording', async () => {
    stopTimer();
    await stopAndTranscribe(false);
  });

  // Hotkey events from Rust (Agent Mode)
  await listen('hotkey-start-agent-recording', async () => {
    console.log('hotkey-start-agent-recording event received');
    setState('agent');
    setMode('agent');
    if (overlayRoot) overlayRoot.classList.remove('active');
    // Capture the foreground app while the overlay is still hidden so the
    // target app owns focus (timing-sensitive).
    const appIcon = loadAppIcon();
    try {
      const prefs = await getRecordingPreferences();
      await invoke('start_recording', { deviceId: prefs.audioDeviceId });
      await appIcon;
      setOverlayCorner(prefs.overlayPosition);
      await invoke('show_overlay', { position: prefs.overlayPosition });

      // Trigger opening transition immediately
      if (overlayRoot) overlayRoot.classList.add('active');
      startTimer();
    } catch (err) {
      console.error('Failed to start/show recording (agent):', err);
      setState('idle');
    }
  });

  await listen('hotkey-stop-agent-recording', async () => {
    stopTimer();
    await stopAndTranscribe(true);
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

async function loadAppIcon() {
  const pill = document.getElementById('app-pill');
  const pillIcon = document.getElementById('app-pill-icon');
  const pillName = document.getElementById('app-pill-name');
  if (!pill || !pillIcon || !pillName) return;
  pill.hidden = true;
  try {
    const info = await invoke('get_foreground_app_icon');
    if (info && info.name && info.icon_data_url) {
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
  if (pillName.textContent === info.name) return;
  pillIcon.src = info.icon_data_url || pillIcon.src;
  pillIcon.alt = `${info.name} icon`;
  pillName.textContent = info.name;
  pill.hidden = false;
  // Apply synchronously — requestAnimationFrame can be dropped while the
  // overlay window is still hidden, which would leave the pill at opacity 0.
  pill.classList.add('visible');
}

// Poll the foreground app while recording so the pill tracks app switches.
function startAppPolling() {
  stopAppPolling();
  appPollTimer = setInterval(async () => {
    try {
      const info = await invoke('get_foreground_app_icon');
      if (info && info.name) applyAppInfo(info);
    } catch (err) {
      // Transient — keep showing the current pill.
    }
  }, 1200);
}

function stopAppPolling() {
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
  clearAutoDismiss();
  retryTimer = setTimeout(() => { fadeAndHide(); }, ms);
}

function setupRetryButton() {
  if (statusRetry) {
    statusRetry.addEventListener('click', async () => {
      clearAutoDismiss();
      hideRetry();
      setStatusMessage('');
      setState('transcribing');
      if (recLabel) recLabel.textContent = 'PROCESSING';
      await runSttFlow();
    });
  }
}

// ── Recording Flow ──────────────────────────────────────────────

function setupDiscardButton() {
  if (cardDiscard) {
    cardDiscard.addEventListener('click', async (e) => {
      e.stopPropagation();
      stopTimer();
      try {
        await invoke('stop_recording');
      } catch (err) {
        console.error('Stop recording failed:', err);
      }
      await fadeAndHide();
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
    audioDeviceId: cachedSettings.audio_device_id || null,
  };
}

async function fadeAndHide() {
  stopTimer();
  clearAutoDismiss();
  hideRetry();
  setStatusMessage('');
  hideAppPill();
  if (overlayRoot) {
    overlayRoot.classList.remove('active');
  }
  await new Promise(r => setTimeout(r, 200)); // let CSS exit transition finish
  await invoke('hide_overlay');
  setState('idle');
}

async function stopAndTranscribe(agentMode) {
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
        await fadeAndHide();
        return;
      }

      const selection = await selectionPromise;
      await handleAgentMode(result.text, settings, result.durationMs || (Date.now() - startTs), selection);
    } else {
      await runSttFlow();
    }
  } catch (err) {
    console.error('Transcription error:', err);
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

async function runSttFlow() {
  const startTs = Date.now();
  try {
    const result = await invoke('finish_transcription_flow');

    const hasAlphanumeric = /[\p{L}\p{N}]/u.test(result.text || '');
    if (!result.text || !result.text.trim() || !hasAlphanumeric) {
      await fadeAndHide();
      return;
    }

    invoke('save_history_entry', {
      text: result.text,
      mode: 'transcription',
      durationMs: result.durationMs || (Date.now() - startTs),
      provider: result.provider,
    }).catch(err => console.error('Failed to save history entry:', err));

    try {
      await invoke('inject_text', { text: result.text });
      setState('success');
      setStatusMessage('Inserted');
      await new Promise(r => setTimeout(r, 1000));
      await fadeAndHide();
    } catch (err) {
      console.error('Inject failed:', err);
      setState('error');
      setStatusMessage('Insert failed');
      showRetry();
      scheduleAutoDismiss(4000);
    }
  } catch (err) {
    console.error('Transcription error:', err);
    setState('error');
    setStatusMessage('Transcription failed');
    showRetry();
    scheduleAutoDismiss(4000);
  }
}

async function handleAgentMode(voiceCommand, settings, durationMs, preGrabbedSelection) {
  try {
    const llmPreset = settings.llm_provider.preset || 'groq';
    const llmTarget = `Fluence/LLM_ApiKey/${llmPreset.toLowerCase().replace(/ /g, '_')}`;
    const llmKey = await invoke('get_api_key', {
      target: llmTarget
    }).catch(() => '');

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

    if (action.action === 'copy') {
      try {
        await navigator.clipboard.writeText(action.content || '');
      } catch (err) {
        console.error('Failed to copy to clipboard:', err);
      }
      setState('success');
      setStatusMessage('Copied');
      await new Promise(r => setTimeout(r, 900));
      await fadeAndHide();
    } else {
      let executed = false;
      if (action.action === 'insert' || action.action === 'rewrite') {
        const textToInsert = action.content || '';
        await invoke('inject_text', { text: textToInsert });
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

      setState('success');
      setStatusMessage(executed ? 'Executed' : 'Done');
      await new Promise(r => setTimeout(r, 800));
      await fadeAndHide();
    }

    invoke('save_history_entry', {
      text: `[Agent] ${voiceCommand}`,
      mode: 'agent',
      durationMs,
      provider: settings.llm_provider.preset,
    }).catch(err => console.error('Failed to save history entry:', err));

  } catch (err) {
    console.error('Agent Error:', err);
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
        recLabel.textContent = 'REC';
        break;
      case 'transcribing':
      case 'agent_transcribing':
        recLabel.textContent = 'PROCESSING';
        break;
      default:
        recLabel.textContent = 'REC';
    }
  }
}
