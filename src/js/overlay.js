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

// State
let currentState = 'idle';
let aura = null;
let cachedSettings = null;
let timerInterval = null;
let recordingStartTime = 0;

// ── Initialization ──────────────────────────────────────────────

window.addEventListener('DOMContentLoaded', () => {
  aura = new AuraVisualizer('waveform-canvas');
  setupEventListeners();
  setupDiscardButton();
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
    try {
      const prefs = await getRecordingPreferences();
      await invoke('start_recording', { deviceId: prefs.audioDeviceId });
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
    try {
      const prefs = await getRecordingPreferences();
      await invoke('start_recording', { deviceId: prefs.audioDeviceId });
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

      fadeAndHide();
      await invoke('inject_text', { text: result.text });
    }
  } catch (err) {
    console.error('Transcription error:', err);
    setState('error');
    await new Promise(r => setTimeout(r, 1500));
    await fadeAndHide();
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
      await new Promise(r => setTimeout(r, 1000));
      await fadeAndHide();
    } else {
      fadeAndHide();

      if (action.action === 'insert' || action.action === 'rewrite') {
        const textToInsert = action.content || '';
        await invoke('inject_text', { text: textToInsert });
      } else if (action.action === 'delete_chars') {
        await invoke('execute_keyboard_action', {
          action: 'delete_chars',
          charCount: action.char_count || 0,
        });
      } else if (action.action === 'select_all') {
        await invoke('execute_keyboard_action', { action: 'select_all', charCount: null });
      } else if (action.action === 'submit') {
        await invoke('execute_keyboard_action', { action: 'submit', charCount: null });
      }
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
    await new Promise(r => setTimeout(r, 1500));
    await fadeAndHide();
  }
}

// ── State Management ────────────────────────────────────────────

function setState(state) {
  currentState = state;

  if (aura) aura.setState(state);

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
