/**
 * Fluence Windows — Overlay Window JS
 * 
 * Controls the floating recording overlay states and connects to
 * Tauri IPC events from the Rust backend.
 * 
 * Design: Stitch-generated Fluence Aura Dark glassmorphic pill.
 */

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

// DOM refs
const overlayRoot  = document.getElementById('overlay-root');

// State
let currentState = 'idle';
let aura = null;
let cachedSettings = null;

// ── Initialization ──────────────────────────────────────────────

window.addEventListener('DOMContentLoaded', () => {
  aura = new AuraVisualizer('waveform-canvas');
  setupEventListeners();
  setState('idle');
});

// ── Tauri Event Listeners ───────────────────────────────────────

async function setupEventListeners() {

  // Hotkey events from Rust (Transcription Mode)
  await listen('hotkey-start-recording', async () => {
    console.log('hotkey-start-recording event received');
    setState('recording');
    if (overlayRoot) overlayRoot.classList.remove('active');
    try {
      const prefs = await getRecordingPreferences();
      await invoke('start_recording', { deviceId: prefs.audioDeviceId });
      await invoke('show_overlay', { position: prefs.overlayPosition });
      
      // Trigger opening transition immediately
      if (overlayRoot) overlayRoot.classList.add('active');
    } catch (err) {
      console.error('Failed to start/show recording:', err);
      setState('idle');
    }
  });

  await listen('hotkey-stop-recording', async () => {
    await stopAndTranscribe(false);
  });

  // Hotkey events from Rust (Agent Mode)
  await listen('hotkey-start-agent-recording', async () => {
    console.log('hotkey-start-agent-recording event received');
    setState('agent');
    if (overlayRoot) overlayRoot.classList.remove('active');
    try {
      const prefs = await getRecordingPreferences();
      await invoke('start_recording', { deviceId: prefs.audioDeviceId });
      await invoke('show_overlay', { position: prefs.overlayPosition });
      
      // Trigger opening transition immediately
      if (overlayRoot) overlayRoot.classList.add('active');
    } catch (err) {
      console.error('Failed to start/show recording (agent):', err);
      setState('idle');
    }
  });

  await listen('hotkey-stop-agent-recording', async () => {
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

// ── Recording Flow ──────────────────────────────────────────────

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
  if (overlayRoot) {
    overlayRoot.classList.remove('active');
  }
  await new Promise(r => setTimeout(r, 200)); // let CSS exit transition finish
  await invoke('hide_overlay');
  setState('idle');
}

async function stopAndTranscribe(agentMode) {
  setState(agentMode ? 'agent_transcribing' : 'transcribing');

  let startTs = Date.now();

  try {
    if (agentMode) {
      console.time('StopAndTranscribeAgent');
      // Grab the active text selection immediately, before ASR starts, to prevent losing focus/highlight context
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

      // Save history entry in database asynchronously (non-blocking)
      invoke('save_history_entry', {
        text: result.text,
        mode: 'transcription',
        durationMs: result.durationMs || (Date.now() - startTs),
        provider: result.provider,
      }).catch(err => console.error('Failed to save history entry:', err));

      // Start fading out the overlay concurrently
      fadeAndHide();

      // Inject text immediately while fade transition is occurring
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

    // Get clipboard context or grab selection
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
      // Start fading out overlay in parallel
      fadeAndHide();

      // Execute the action immediately while fade transition is occurring
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

    // Save history entry asynchronously (non-blocking)
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
}
