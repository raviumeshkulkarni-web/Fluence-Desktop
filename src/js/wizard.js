/**
 * Fluence Windows — Setup Wizard JavaScript
 * 
 * Controls the 6-step first-run onboarding wizard:
 * 1. Welcome
 * 2. API Key Setup
 * 3. Hotkey Configuration
 * 4. Overlay Position
 * 5. Test Recording
 * 6. Complete
 */

const { invoke } = window.__TAURI__.core;

// ── State ─────────────────────────────────────────────────────────

const TOTAL_STEPS = 6;
let currentStep = 1;
let wizardData = {
  provider: 'groq',
  baseUrl: 'https://api.groq.com/openai',
  apiKey: '',
  model: 'whisper-large-v3',
  llmModel: 'llama-3.3-70b-versatile',
  hotkey: 'Ctrl+Shift+Space',
  recordingMode: 'push_to_toggle',
  overlayPosition: 'bottom_right',
};

let isRecordingHotkey = false;
let testRecording = false;

const PROVIDER_PRESETS = {
  groq:   { url: 'https://api.groq.com/openai', model: 'whisper-large-v3', llmModel: 'llama-3.3-70b-versatile' },
  openai: { url: 'https://api.openai.com',      model: 'whisper-1',        llmModel: 'gpt-4o' },
  custom: { url: '',                             model: '',                 llmModel: '' },
};

// ── Init ──────────────────────────────────────────────────────────

window.addEventListener('DOMContentLoaded', () => {
  updateStep(1);
  setupTitlebar();
  setupNavButtons();
  setupStep2();
  setupStep3();
  setupStep4();
  setupStep5();
  setupStep6();
});

// ── Titlebar ──────────────────────────────────────────────────────

function setupTitlebar() {
  const minimizeBtn = document.getElementById('titlebar-minimize');
  const closeBtn = document.getElementById('titlebar-close');
  if (minimizeBtn) minimizeBtn.addEventListener('click', () => {
    invoke('minimize_wizard').catch(err => console.error('Failed to minimize wizard:', err));
  });
  if (closeBtn) closeBtn.addEventListener('click', () => {
    invoke('close_wizard').catch(err => console.error('Failed to close wizard:', err));
  });
}

// ── UI / Navigation ─────────────────────────────────────────────────────

function setupNavButtons() {
  document.getElementById('prev-btn')?.addEventListener('click', () => {
    if (currentStep > 1) updateStep(currentStep - 1);
  });

  document.getElementById('next-btn')?.addEventListener('click', async () => {
    if (await validateCurrentStep()) {
      if (currentStep < TOTAL_STEPS) {
        updateStep(currentStep + 1);
      }
    }
  });
}

function updateStep(step) {
  // Animate out current
  const currentEl = document.getElementById(`step-${currentStep}`);
  const isForward = step > currentStep;

  if (currentEl && step !== currentStep) {
    currentEl.classList.remove('active', 'exit-left', 'exit-right', 'enter-left', 'enter-right');
    if (isForward) {
      currentEl.classList.add('exit-left');
    } else {
      currentEl.classList.add('exit-right');
    }
    const oldStep = currentEl;
    setTimeout(() => {
      oldStep.classList.remove('exit-left', 'exit-right');
    }, 350);
  }

  currentStep = step;

  // Animate in new
  const nextEl = document.getElementById(`step-${step}`);
  if (nextEl) {
    nextEl.classList.remove('active', 'enter-left', 'enter-right', 'exit-left', 'exit-right');
    if (isForward) {
      nextEl.classList.add('enter-right');
    } else {
      nextEl.classList.add('enter-left');
    }

    // Force reflow
    nextEl.offsetHeight;

    setTimeout(() => {
      nextEl.classList.add('active');
      nextEl.classList.remove('enter-left', 'enter-right');
    }, 30);
  }

  // Update progress bar
  const progress = ((step - 1) / (TOTAL_STEPS - 1)) * 100;
  const fill = document.getElementById('progress-fill');
  if (fill) fill.style.width = `${progress}%`;

  const track = document.querySelector('.progress-bar-track');
  if (track) {
    track.style.opacity = (step === 1) ? '0' : '1';
  }

  // Update dots
  document.querySelectorAll('.step-dot').forEach(dot => {
    const dotStep = parseInt(dot.dataset.dot);
    dot.classList.remove('active', 'done');
    if (dotStep === step) dot.classList.add('active');
    else if (dotStep < step) dot.classList.add('done');
  });

  // Update nav buttons
  const prevBtn = document.getElementById('prev-btn');
  const nextBtn = document.getElementById('next-btn');

  if (prevBtn) prevBtn.style.visibility = (step > 1 && step < TOTAL_STEPS) ? 'visible' : 'hidden';

  if (nextBtn) {
    if (step === TOTAL_STEPS) {
      nextBtn.classList.add('hidden');
    } else {
      nextBtn.classList.remove('hidden');
      nextBtn.textContent = step === 1 ? 'Get Started' : (step === TOTAL_STEPS - 1 ? 'Finish Setup' : 'Continue →');
    }
  }
}

async function validateCurrentStep() {
  if (currentStep === 2) {
    const key = document.getElementById('wiz-api-key')?.value?.trim();
    if (!key) {
      showStepError('Please enter your API key to continue');
      return false;
    }
    wizardData.apiKey = key;
    wizardData.baseUrl = document.getElementById('wiz-base-url')?.value?.trim() || wizardData.baseUrl;
    wizardData.model = document.getElementById('wiz-model-select')?.value || wizardData.model;
    wizardData.llmModel = document.getElementById('wiz-llm-model-select')?.value || wizardData.llmModel;
    // Save key
    try {
      await invoke('save_api_key', { target: 'Fluence/STT_ApiKey', key });
      await invoke('save_api_key', { target: 'Fluence/LLM_ApiKey', key }); // Same key for now
    } catch (err) {
      showStepError('Failed to save API key: ' + err);
      return false;
    }
  }
  return true;
}

function showStepError(message) {
  // Briefly shake the next button and show inline message
  const nextBtn = document.getElementById('next-btn');
  if (nextBtn) {
    nextBtn.style.boxShadow = '0 0 0 3px rgba(255,100,100,0.4)';
    setTimeout(() => nextBtn.style.boxShadow = '', 1000);
  }
  // Could also show inline error — simple alert for now
  console.warn(message);
}

// ── Step 2: API Key ────────────────────────────────────────────────

function setupStep2() {
  // Provider cards
  document.querySelectorAll('[data-provider]').forEach(card => {
    card.addEventListener('click', () => {
      document.querySelectorAll('[data-provider]').forEach(c => c.classList.remove('selected'));
      card.classList.add('selected');

      const preset = card.dataset.provider;
      wizardData.provider = preset;

      const p = PROVIDER_PRESETS[preset];
      if (p) {
        setInputValue('wiz-base-url', p.url);
        setSelectOption('wiz-model-select', p.model);
        setSelectOption('wiz-llm-model-select', p.llmModel);
        wizardData.baseUrl = p.url;
        wizardData.model = p.model;
        wizardData.llmModel = p.llmModel;
      }

      // Show/hide endpoint for custom
      const endpointRow = document.getElementById('wiz-endpoint-row');
      if (endpointRow) endpointRow.style.display = (preset === 'custom') ? 'flex' : 'none';
    });
  });

  // Fetch models
  const doFetchModels = async () => {
    const baseUrl = document.getElementById('wiz-base-url')?.value?.trim();
    const apiKey = document.getElementById('wiz-api-key')?.value?.trim();
    if (!baseUrl || !apiKey || apiKey.length < 8) return;

    const btn1 = document.getElementById('wiz-fetch-models-btn');
    const btn2 = document.getElementById('wiz-fetch-llm-models-btn');
    btn1?.classList.add('animate-spin');
    btn2?.classList.add('animate-spin');
    try {
      const models = await invoke('fetch_models', { baseUrl, apiKey });
      
      const selectSTT = document.getElementById('wiz-model-select');
      if (selectSTT) {
        selectSTT.textContent = '';
        models.forEach(m => {
          const opt = document.createElement('option');
          opt.value = m;
          opt.textContent = m;
          selectSTT.appendChild(opt);
        });
        if (wizardData.model && models.includes(wizardData.model)) selectSTT.value = wizardData.model;
      }

      const selectLLM = document.getElementById('wiz-llm-model-select');
      if (selectLLM) {
        selectLLM.textContent = '';
        models.forEach(m => {
          const opt = document.createElement('option');
          opt.value = m;
          opt.textContent = m;
          selectLLM.appendChild(opt);
        });
        if (wizardData.llmModel && models.includes(wizardData.llmModel)) selectLLM.value = wizardData.llmModel;
      }
    } catch (err) {
      console.error('Fetch models failed:', err);
    } finally {
      btn1?.classList.remove('animate-spin');
      btn2?.classList.remove('animate-spin');
    }
  };

  document.getElementById('wiz-fetch-models-btn')?.addEventListener('click', doFetchModels);
  document.getElementById('wiz-fetch-llm-models-btn')?.addEventListener('click', doFetchModels);

  let fetchTimeout;
  document.getElementById('wiz-api-key')?.addEventListener('input', () => {
    clearTimeout(fetchTimeout);
    fetchTimeout = setTimeout(doFetchModels, 800);
  });

  // Test connection
  document.getElementById('wiz-test-btn')?.addEventListener('click', async () => {
    const baseUrl = document.getElementById('wiz-base-url')?.value?.trim();
    const apiKey = document.getElementById('wiz-api-key')?.value?.trim();
    const dot = document.querySelector('#wiz-test-status .dot');
    const txt = document.getElementById('wiz-test-text');

    if (dot) dot.className = 'dot dot-idle';
    if (txt) txt.textContent = 'Testing...';

    try {
      const msg = await invoke('test_stt_connection', { baseUrl, apiKey });
      if (dot) dot.className = 'dot dot-success';
      if (txt) txt.textContent = msg;
    } catch (err) {
      if (dot) dot.className = 'dot dot-error';
      if (txt) txt.textContent = String(err).replace('Error: ', '');
    }
  });

  setTimeout(async () => {
    const key = await invoke('get_api_key', { target: 'Fluence/STT_ApiKey' }).catch(() => null);
    if (key) {
      const input = document.getElementById('wiz-api-key');
      if (input && !input.value) input.value = key;
      doFetchModels();
    }
  }, 500);
}

// ── Step 3: Hotkey ─────────────────────────────────────────────────

function setupStep3() {
  const display = document.getElementById('wiz-hotkey-display');

  display?.addEventListener('click', () => {
    if (isRecordingHotkey) return;
    isRecordingHotkey = true;
    display.classList.add('recording');
    document.getElementById('wiz-hotkey-text').textContent = 'Press your shortcut...';
  });

  document.addEventListener('keydown', (e) => {
    if (!isRecordingHotkey || currentStep !== 3) return;
    e.preventDefault();

    if (e.key === 'Escape') {
      isRecordingHotkey = false;
      display?.classList.remove('recording');
      document.getElementById('wiz-hotkey-text').textContent = wizardData.hotkey;
      return;
    }

    const parts = [];
    if (e.ctrlKey)  parts.push('Ctrl');
    if (e.altKey)   parts.push('Alt');
    if (e.shiftKey) parts.push('Shift');
    const mods = new Set(['Control','Alt','Shift','Meta']);
    if (!mods.has(e.key)) parts.push(e.key === ' ' ? 'Space' : e.key.length === 1 ? e.key.toUpperCase() : e.key);

    if (parts.length > 0) {
      document.getElementById('wiz-hotkey-text').textContent = parts.join('+');
    }
  });

  document.addEventListener('keyup', (e) => {
    if (!isRecordingHotkey || currentStep !== 3) return;
    const current = document.getElementById('wiz-hotkey-text')?.textContent;
    if (current && current !== 'Press your shortcut...') {
      wizardData.hotkey = current;
      isRecordingHotkey = false;
      display?.classList.remove('recording');
      document.getElementById('done-hotkey').textContent = current;
    }
  });

  // Mode selector
  document.querySelectorAll('.mode-option').forEach(btn => {
    btn.addEventListener('click', () => {
      document.querySelectorAll('.mode-option').forEach(b => b.classList.remove('selected'));
      btn.classList.add('selected');
      wizardData.recordingMode = btn.dataset.mode;
    });
  });
}

// ── Step 4: Position ───────────────────────────────────────────────

function setupStep4() {
  document.querySelectorAll('.position-option').forEach(btn => {
    btn.addEventListener('click', () => {
      document.querySelectorAll('.position-option').forEach(b => b.classList.remove('selected'));
      btn.classList.add('selected');
      wizardData.overlayPosition = btn.dataset.pos;
    });
  });
}

// ── Step 5: Test Recording ─────────────────────────────────────────

function setupStep5() {
  let isTestRecording = false;
  const testBtn = document.getElementById('wiz-test-record-btn');
  const testResult = document.getElementById('wiz-test-result');

  testBtn?.addEventListener('click', async () => {
    if (!isTestRecording) {
      isTestRecording = true;
      testBtn.textContent = '⏹ Stop Recording';
      testBtn.style.background = 'linear-gradient(135deg, #ef4444, #c0392b)';
      if (testResult) { testResult.className = 'test-result'; testResult.textContent = 'Recording... speak now'; }

      try {
        await invoke('start_recording', { deviceId: null });
      } catch (err) {
        isTestRecording = false;
        testBtn.textContent = '🎙 Start Recording';
        testBtn.style.background = '';
        if (testResult) { testResult.textContent = 'Failed to start: ' + err; }
      }
    } else {
      isTestRecording = false;
      testBtn.textContent = '⏳ Transcribing...';
      testBtn.disabled = true;

      try {
        const wavB64 = await invoke('stop_recording');
        const text = await invoke('transcribe_audio', {
          req: {
            base_url: wizardData.baseUrl,
            api_key: wizardData.apiKey,
            model: wizardData.model,
            wav_b64: wavB64,
            language: 'en',
          }
        });
        if (testResult) {
          testResult.className = 'test-result';
          testResult.textContent = text || '(empty transcription)';
        }
      } catch (err) {
        if (testResult) {
          testResult.className = 'test-result';
          testResult.textContent = 'Error: ' + err;
        }
      } finally {
        testBtn.textContent = '🎙 Try Again';
        testBtn.style.background = '';
        testBtn.disabled = false;
      }
    }
  });
}

// ── Step 6: Done ───────────────────────────────────────────────────

function setupStep6() {
  document.getElementById('wiz-open-settings-btn')?.addEventListener('click', async () => {
    await saveWizardSettings();
    await invoke('close_wizard');
    await invoke('show_main_window');
  });

  document.getElementById('wiz-close-btn')?.addEventListener('click', async () => {
    await saveWizardSettings();
    await invoke('close_wizard');
  });
}

async function saveWizardSettings() {
  try {
    const settings = {
      hotkey: wizardData.hotkey,
      recording_mode: wizardData.recordingMode,
      overlay_position: wizardData.overlayPosition,
      stt_provider: {
        preset: wizardData.provider,
        base_url: wizardData.baseUrl,
        model: wizardData.model,
        api_key_saved: true,
      },
      llm_provider: {
        preset: wizardData.provider,
        base_url: wizardData.baseUrl,
        model: wizardData.llmModel,
        api_key_saved: true,
      },
      auto_start: false,
      sound_on_complete: true,
      agent_mode_threshold_ms: 800,
      language: 'en',
      agent_hotkey: 'Ctrl+Shift+A',
      agent_recording_mode: 'push_to_toggle',
      ai_polish_style: 'none',
      auto_grab_highlight: true,
      audio_device_id: null,
      theme: 'dark',
      first_run: false,
    };
    await invoke('update_settings', { settings });
    await invoke('update_hotkeys', {
      transcriptionShortcut: wizardData.hotkey,
      transcriptionMode: wizardData.recordingMode,
      agentShortcut: 'Ctrl+Shift+A',
      agentMode: 'push_to_toggle',
    });
  } catch (err) {
    console.error('Failed to save wizard settings:', err);
  }
}

// ── Helpers ────────────────────────────────────────────────────────

function setInputValue(id, value) {
  const el = document.getElementById(id);
  if (el) el.value = value;
}

function setSelectOption(id, value) {
  const el = document.getElementById(id);
  if (!el) return;
  if (!Array.from(el.options).find(o => o.value === value)) {
    const opt = document.createElement('option');
    opt.value = value;
    opt.textContent = value;
    el.appendChild(opt);
  }
  el.value = value;
}
