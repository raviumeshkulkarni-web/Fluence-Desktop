/**
 * Fluence Windows - Settings Page JavaScript
 * 
 * Handles all settings page interactions:
 * - Navigation between tabs
 * - Hotkey recording
 * - Provider configuration with dynamic model fetching
 * - Transcription history
 * - Auto-start and system toggles
 * 
 * Design: Precision Ink - Clean, matte, monochrome-first
 */

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

// ── State ───────────────────────────────────────────────────────
let currentSettings = null;
let currentPage = 'dashboard';
let historyPage = 0;
let activeRecorder = null;
let pendingHotkey = '';
let pendingHotkeyKeys = new Set();
const MODIFIER_KEYS = new Set(['Control', 'Alt', 'Shift', 'Meta']);
let dictEntries = [];
let suggestionsLoading = false;
let historyGroupKey = null;
let historySearchQuery = '';

// Provider presets
const STT_PRESETS = {
  groq:    { base_url: 'https://api.groq.com/openai',   model: 'whisper-large-v3' },
  openai:  { base_url: 'https://api.openai.com',        model: 'whisper-1' },
  mistral: { base_url: 'https://api.mistral.ai',        model: 'mistral-stt' },
  custom:  { base_url: '',                              model: '' },
  'Local Offline': { base_url: '',                      model: '' },
};

const LLM_PRESETS = {
  groq:    { base_url: 'https://api.groq.com/openai',   model: 'llama-3.3-70b-versatile' },
  openai:  { base_url: 'https://api.openai.com',        model: 'gpt-4o' },
  mistral: { base_url: 'https://api.mistral.ai',        model: 'mistral-large-latest' },
  custom:  { base_url: '',                              model: '' },
};

// ── Boot ─────────────────────────────────────────────────────────

window.addEventListener('DOMContentLoaded', async () => {
  await loadSettings();
  setupTitlebar();
  setupNavigation();
  setupHotkeyRecorders();
  setupProviderCards();
  setupOfflineDownloader();
  setupHistory();
  setupAutoApply();
  setupSaveButtons();
  setupDictionary();
  setupSuggestions();
  setupSnippets();
  setupSyncPage();
  populateAudioDevices();
  listenForTauriEvents();
  loadAppVersion();
  setupSkeletonLoading();
  loadDashboardStats().finally(removeSkeletonLoading);
  setupUpdaterUI();
  setupKeyboardShortcuts();

  // Refresh data when window is focused
  window.addEventListener('focus', () => {
    if (currentPage === 'dashboard') {
      loadDashboardStats();
    } else if (currentPage === 'history') {
      loadHistory(true);
    } else if (currentPage === 'dictionary') {
      loadDictionary();
      loadSuggestions();
      expireStaleSuggestions();
    } else if (currentPage === 'snippets') {
      loadSnippets();
    }
  });
});

// ── Titlebar ──────────────────────────────────────────────────────

function setupTitlebar() {
  const minimizeBtn = document.getElementById('titlebar-minimize');
  const maximizeBtn = document.getElementById('titlebar-maximize');
  const closeBtn = document.getElementById('titlebar-close');

  if (minimizeBtn) minimizeBtn.addEventListener('click', () => {
    invoke('minimize_main_window').catch(err => console.error('Failed to minimize:', err));
  });
  if (maximizeBtn) maximizeBtn.addEventListener('click', () => {
    invoke('toggle_maximize_main_window')
      .then((maximized) => {
        const svgEl = maximizeBtn.querySelector('svg');
        if (svgEl) {
          svgEl.outerHTML = maximized ? RESTORE_SVG : MAXIMIZE_SVG;
        }
        maximizeBtn.setAttribute('aria-label', maximized ? 'Restore window' : 'Maximize window');
        maximizeBtn.setAttribute('title', maximized ? 'Restore window' : 'Maximize window');
      })
      .catch(err => console.error('Failed to toggle maximize:', err));
  });
  if (closeBtn) closeBtn.addEventListener('click', () => {
    // Hide instead of close so app stays in tray
    try { sessionStorage.setItem('fluence-tray-hint', '1'); } catch {}
    invoke('hide_main_window').catch(err => console.error('Failed to hide:', err));
  });
  // One-time reassurance when the window comes back from the tray.
  document.addEventListener('visibilitychange', () => {
    if (document.visibilityState !== 'visible') return;
    let flagged = false;
    try { flagged = sessionStorage.getItem('fluence-tray-hint') === '1'; } catch {}
    if (!flagged) return;
    try { sessionStorage.removeItem('fluence-tray-hint'); } catch {}
    showToast('Fluence stayed in the tray and kept your hotkey active', 'info');
  });
}

const MAXIMIZE_SVG = '<svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true"><rect x="0.75" y="0.75" width="8.5" height="8.5" rx="1" fill="none" stroke="currentColor" stroke-width="1.2"/></svg>';
const RESTORE_SVG = '<svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true"><rect x="2.25" y="0.75" width="7" height="7" rx="1" fill="none" stroke="currentColor" stroke-width="1.2"/><rect x="0.75" y="2.75" width="6.5" height="6.5" rx="1" fill="none" stroke="currentColor" stroke-width="1.2"/></svg>';

// ── Settings Loading ─────────────────────────────────────────────

async function loadSettings() {
  try {
    currentSettings = await invoke('get_settings');
    populateUI(currentSettings);
  } catch (err) {
    showToast('Failed to load settings: ' + err, 'error');
  }
}

function populateUI(s) {
  // General tab
  setText('hotkey-display-text', s.hotkey || 'Ctrl+Shift+Space');
  setSelectValue('recording-mode-select', s.recording_mode || 'push_to_toggle');
  setText('agent-hotkey-display-text', s.agent_hotkey || 'Ctrl+Shift+A');
  setSelectValue('agent-recording-mode-select', s.agent_recording_mode || 'push_to_toggle');
  setSelectValue('overlay-position-select', s.overlay_position || 'bottom_right');
  setSelectValue('overlay-style-select', s.overlay_style || 'full');
  setSelectValue('language-select', s.language || 'en');
  setChecked('autostart-cb', s.auto_start || false);
  setChecked('duck-cb', s.duck_enabled || false);
  setSelectValue('ai-polish-select', s.ai_polish_style || 'none');
  setChecked('auto-grab-cb', s.auto_grab_highlight !== false);
  setChecked('auto-learn-cb', s.auto_learn_enabled !== false);
  setChecked('auto-accept-cb', s.auto_accept_enabled === true);
  syncLearnAcceptUI();
  setChecked('sound-on-complete-cb', s.sound_on_complete ?? true);
  // Retired Moonshine v1 id maps to its English successor (same mapping
  // the backend applies at load; belt-and-braces for any stale payload).
  const offlineEngine = s.offline_engine === 'moonshine_base' ? 'moonshine_v2_small' : (s.offline_engine || 'sensevoice');
  selectOfflineEngineCard(offlineEngine);

  // Sync tab
  setChecked('sync-enabled-cb', s.sync_enabled || false);

  // Providers tab
  const sttPreset = s.stt_provider?.preset || 'groq';
  selectProviderCard('stt', sttPreset);
  updateSttUiVisibility(sttPreset);
  setInputValue('stt-base-url', s.stt_provider?.base_url || '');
  setSelectOption('stt-model-select', s.stt_provider?.model || 'whisper-large-v3');

  const llmPreset = s.llm_provider?.preset || 'groq';
  selectProviderCard('llm', llmPreset);
  setInputValue('llm-base-url', s.llm_provider?.base_url || '');
  setSelectOption('llm-model-select', s.llm_provider?.model || '');

  setTimeout(async () => {
    // Populate keys for currently selected presets specifically
    const sttTarget = `Fluence/STT_ApiKey/${sttPreset.toLowerCase().replace(/ /g, '_')}`;
    const sttKey = await invoke('get_api_key', { target: sttTarget }).catch(() => null);
    if (sttKey) fetchModels('stt', true);
    
    const llmTarget = `Fluence/LLM_ApiKey/${llmPreset.toLowerCase().replace(/ /g, '_')}`;
    const llmKey = await invoke('get_api_key', { target: llmTarget }).catch(() => null);
    if (llmKey) fetchModels('llm', true);
  }, 500);
}

// ── Navigation ───────────────────────────────────────────────────

const PAGE_ORDER = ['dashboard', 'history', 'general', 'providers', 'dictionary', 'snippets', 'sync', 'about'];

function setupNavigation() {
  document.querySelectorAll('.nav-item').forEach(item => {
    item.addEventListener('click', () => {
      const page = item.dataset.page;
      if (page) navigateTo(page);
    });
  });

  // Listen for tray navigate events
  listen('navigate-to', (evt) => {
    lastTrayNavigateAt = Date.now();
    navigateTo(evt.payload);
  });
}

// Timestamp of the last tray-driven navigation, so the show-handler below
// can tell an explicit target (e.g. History) apart from a plain open.
let lastTrayNavigateAt = 0;

function navigateTo(page) {
  if (currentPage === page) return;
  // Ignore unknown targets (e.g. stale tray events) instead of blanking the UI.
  if (PAGE_ORDER.indexOf(page) === -1) return;

  const currentIndex = PAGE_ORDER.indexOf(currentPage);
  const targetIndex = PAGE_ORDER.indexOf(page);
  const direction = targetIndex > currentIndex ? 'forward' : 'backward';

  const htmlEl = document.documentElement;
  htmlEl.classList.add(`nav-${direction}`);

  const updateDOM = () => {
    _performNavigation(page);
  };

  if (document.startViewTransition) {
    const transition = document.startViewTransition(updateDOM);
    transition.finished.finally(() => {
      htmlEl.classList.remove(`nav-${direction}`);
    });
  } else {
    updateDOM();
    htmlEl.classList.remove(`nav-${direction}`);
  }
}

function _performNavigation(page) {
  currentPage = page;

  document.querySelectorAll('.nav-item').forEach(item => {
    const isActive = item.dataset.page === page;
    item.classList.toggle('active', isActive);
    if (isActive) item.setAttribute('aria-current', 'page');
    else item.removeAttribute('aria-current');
  });
  document.querySelectorAll('.page').forEach(p => {
    p.classList.toggle('active', p.id === `page-${page}`);
  });

  // Move focus to the new page's title for assistive technology
  document.querySelector(`#page-${page} .page-title`)?.focus();

  // Lazy load data for specific pages
  // Dashboard owns stats + chart; History owns the transcription list.
  // No backend calls changed — same get_account_stats / get_history /
  // get_weekly_activity commands, only split by visible page.
  if (page === 'dashboard') {
    loadDashboardStats();
  }
  if (page === 'history') {
    loadHistory(true);
  }
  if (page === 'dictionary') {
    loadDictionary();
    loadSuggestions();
    expireStaleSuggestions();
  }
  if (page === 'snippets') {
    loadSnippets();
  }
  if (page === 'sync') {
    loadSyncPage();
  }
}

// ── Hotkey Recorder ──────────────────────────────────────────────

function setupHotkeyRecorders() {
  wireHotkeyRecorder('hotkey-display', 'hotkey-display-text', 'hotkey-clear-btn', 'hotkey', 'Ctrl+Shift+Space');
  wireHotkeyRecorder('agent-hotkey-display', 'agent-hotkey-display-text', 'agent-hotkey-clear-btn', 'agent_hotkey', 'Ctrl+Shift+A');

  document.addEventListener('keydown', (e) => {
    if (!activeRecorder) return;
    e.preventDefault();

    if (e.key === 'Escape') {
      stopHotkeyRecording(false);
      // Keep the global Esc handler from hiding the window mid-capture
      e.stopImmediatePropagation();
      return;
    }

    pendingHotkeyKeys.add(e.key);
    const parts = buildHotkeyString(e);
    setText(activeRecorder.textId, parts || 'Press keys…');
    pendingHotkey = parts;
  });

  document.addEventListener('keyup', (e) => {
    if (!activeRecorder) return;
    if (MODIFIER_KEYS.has(e.key)) return;
    if (pendingHotkey && pendingHotkeyKeys.size > 0) {
      stopHotkeyRecording(true);
    }
  });

  // Cancel recording when the user clicks outside the active recorder
  document.addEventListener('click', (e) => {
    if (!activeRecorder) return;
    const target = e.target instanceof Element ? e.target : null;
    if (!target) return;
    const insideActive = activeRecorder.displayEl?.contains(target) ||
      activeRecorder.clearEl?.contains(target);
    if (!insideActive) stopHotkeyRecording(false);
  });
}

function wireHotkeyRecorder(displayId, textId, clearBtnId, settingsKey, defaultShortcut) {
  const display = document.getElementById(displayId);
  const clearBtn = document.getElementById(clearBtnId);

  display?.addEventListener('click', () => {
    if (activeRecorder && activeRecorder.displayId === displayId) {
      stopHotkeyRecording(false);
    } else {
      if (activeRecorder) {
        stopHotkeyRecording(false);
      }
      startHotkeyRecording(displayId, textId, settingsKey, clearBtn);
    }
  });

  // Cancel recording if the active display loses focus
  display?.addEventListener('blur', () => {
    if (activeRecorder && activeRecorder.displayId === displayId) {
      stopHotkeyRecording(false);
    }
  });

  // Keyboard activation (Enter/Space) - while capturing, the document
  // recorder owns all keys, so only start recording from here
  display?.addEventListener('keydown', (e) => {
    if (activeRecorder) return;
    if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      e.stopPropagation();
      display.click();
    }
  });

  clearBtn?.addEventListener('click', () => {
    setText(textId, defaultShortcut);
    if (currentSettings) {
      currentSettings[settingsKey] = defaultShortcut;
      queuePersist('hotkeys');
    }
  });
}

function startHotkeyRecording(displayId, textId, settingsKey, clearEl) {
  const display = document.getElementById(displayId);
  activeRecorder = { displayId, textId, settingsKey, displayEl: display, clearEl: clearEl || null };
  pendingHotkeyKeys = new Set();
  pendingHotkey = '';
  display?.classList.add('recording');
  setText(textId, 'Press your shortcut…');
}

function stopHotkeyRecording(apply) {
  if (!activeRecorder) return;

  activeRecorder.displayEl?.classList.remove('recording');

  if (apply && pendingHotkey) {
    setText(activeRecorder.textId, pendingHotkey);
    if (currentSettings) {
      currentSettings[activeRecorder.settingsKey] = pendingHotkey;
      queuePersist('hotkeys');
    }
  } else {
    setText(activeRecorder.textId, currentSettings?.[activeRecorder.settingsKey] || 'Ctrl+Shift+Space');
  }
  activeRecorder = null;
  pendingHotkeyKeys = new Set();
}

function buildHotkeyString(e) {
  const parts = [];
  if (e.ctrlKey)  parts.push('Ctrl');
  if (e.altKey)   parts.push('Alt');
  if (e.shiftKey) parts.push('Shift');
  if (e.metaKey)  parts.push('Meta');

  if (!MODIFIER_KEYS.has(e.key)) {
    parts.push(e.key === ' ' ? 'Space' : e.key.length === 1 ? e.key.toUpperCase() : e.key);
  }
  return parts.join('+');
}

// ── Provider Cards ───────────────────────────────────────────────

// Unsaved key drafts, stashed per preset so switching cards never wipes
// what the user typed. Saved keys live in Credential Manager; these are
// input-only drafts that were never saved.
const keyDrafts = { stt: Object.create(null), llm: Object.create(null) };

function stashKeyDraft(type) {
  const grid = type === 'stt' ? '#stt-provider-grid' : '#llm-provider-grid';
  const current = document.querySelector(`${grid} .provider-card.selected`)?.dataset.provider;
  const val = document.getElementById(`${type}-api-key`)?.value || '';
  if (current && val) keyDrafts[type][current] = val;
}

function restoreKeyDraft(type, preset) {
  const keyInput = document.getElementById(`${type}-api-key`);
  if (!keyInput) return;
  keyInput.value = keyDrafts[type][preset] || '';
  delete keyDrafts[type][preset];
  updateProviderGates(type);
}

// Test/Fetch require a parseable URL + a key-like value. Buttons stay
// enabled but validate inline so the failure explains itself in-status.
function providerReady(type) {
  const baseUrl = document.getElementById(`${type}-base-url`)?.value?.trim() || '';
  const keyVal = document.getElementById(`${type}-api-key`)?.value?.trim() || '';
  let urlOk = false;
  try {
    const u = new URL(baseUrl);
    urlOk = u.protocol === 'http:' || u.protocol === 'https:';
  } catch { urlOk = false; }
  return { urlOk, keyOk: keyVal.length >= 8, baseUrl, keyVal };
}

function updateProviderGates(type) {
  const { urlOk, keyOk } = providerReady(type);
  const ready = urlOk && keyOk;
  for (const id of [`${type}-test-btn`, `${type}-fetch-models-btn`]) {
    const btn = document.getElementById(id);
    if (!btn) continue;
    btn.disabled = !ready;
    btn.title = ready ? '' : 'Enter a valid http(s) URL and an API key (8+ chars) first';
  }
  return ready;
}

function setupProviderCards() {
  // STT cards
  document.querySelectorAll('#stt-provider-grid .provider-card').forEach(card => {
    card.addEventListener('click', async () => {
      const preset = card.dataset.provider;
      stashKeyDraft('stt');
      selectProviderCard('stt', preset);
      updateSttUiVisibility(preset);
      if (preset !== 'custom' && preset !== 'Local Offline') {
        setInputValue('stt-base-url', STT_PRESETS[preset]?.base_url || '');
        setSelectOption('stt-model-select', STT_PRESETS[preset]?.model || '');
      }

      // Restore any unsaved draft for this preset instead of wiping it.
      restoreKeyDraft('stt', preset);

      const target = `Fluence/STT_ApiKey/${preset.toLowerCase().replace(/ /g, '_')}`;
      const hasKey = await invoke('get_api_key', { target }).then(() => true).catch(() => false);
      if (hasKey) {
        fetchModels('stt', true);
      }
      queuePersist('providers');
      updateProviderGates('stt');
    });
  });

  // LLM cards
  document.querySelectorAll('#llm-provider-grid .provider-card').forEach(card => {
    card.addEventListener('click', async () => {
      const preset = card.dataset.provider;
      stashKeyDraft('llm');
      selectProviderCard('llm', preset);
      if (preset !== 'custom') {
        setInputValue('llm-base-url', LLM_PRESETS[preset]?.base_url || '');
        setSelectOption('llm-model-select', LLM_PRESETS[preset]?.model || '');
      }

      restoreKeyDraft('llm', preset);

      const target = `Fluence/LLM_ApiKey/${preset.toLowerCase().replace(/ /g, '_')}`;
      const hasKey = await invoke('get_api_key', { target }).then(() => true).catch(() => false);
      if (hasKey) {
        fetchModels('llm', true);
      }
      queuePersist('providers');
      updateProviderGates('llm');
    });
  });

  // Save key buttons
  document.getElementById('stt-save-key-btn')?.addEventListener('click', async () => {
    const key = document.getElementById('stt-api-key')?.value?.trim();
    if (!key) return showToast('Please enter an API key', 'error');
    
    const preset = document.querySelector('#stt-provider-grid .provider-card.selected')?.dataset.provider || 'groq';
    const target = `Fluence/STT_ApiKey/${preset.toLowerCase().replace(/ /g, '_')}`;
    
    try {
      await invoke('save_api_key', { target, key });
      document.getElementById('stt-api-key').value = '';
      showToast(`${preset} API key saved securely ✓`, 'success');
    } catch (err) {
      showToast('Failed to save key: ' + err, 'error');
    }
  });

  document.getElementById('llm-save-key-btn')?.addEventListener('click', async () => {
    const key = document.getElementById('llm-api-key')?.value?.trim();
    if (!key) return showToast('Please enter an API key', 'error');

    const preset = document.querySelector('#llm-provider-grid .provider-card.selected')?.dataset.provider || 'groq';
    const target = `Fluence/LLM_ApiKey/${preset.toLowerCase().replace(/ /g, '_')}`;

    try {
      await invoke('save_api_key', { target, key });
      document.getElementById('llm-api-key').value = '';
      showToast(`${preset} API key saved securely ✓`, 'success');
    } catch (err) {
      showToast('Failed to save key: ' + err, 'error');
    }
  });

  // Fetch models buttons
  document.getElementById('stt-fetch-models-btn')?.addEventListener('click', () => fetchModels('stt'));
  document.getElementById('llm-fetch-models-btn')?.addEventListener('click', () => fetchModels('llm'));

  // Auto-fetch on API key input
  let sttFetchTimeout;
  document.getElementById('stt-api-key')?.addEventListener('input', () => {
    updateProviderGates('stt');
    clearTimeout(sttFetchTimeout);
    sttFetchTimeout = setTimeout(() => fetchModels('stt', true), 800);
  });

  let llmFetchTimeout;
  document.getElementById('llm-api-key')?.addEventListener('input', () => {
    updateProviderGates('llm');
    clearTimeout(llmFetchTimeout);
    llmFetchTimeout = setTimeout(() => fetchModels('llm', true), 800);
  });

  // Test buttons
  document.getElementById('stt-test-btn')?.addEventListener('click', () => testConnection('stt'));
  document.getElementById('llm-test-btn')?.addEventListener('click', () => testConnection('llm'));

  // Auto-apply provider endpoint/model changes (debounced)
  document.getElementById('stt-base-url')?.addEventListener('input', () => { queuePersist('providers'); updateProviderGates('stt'); });
  document.getElementById('llm-base-url')?.addEventListener('input', () => { queuePersist('providers'); updateProviderGates('llm'); });
  document.getElementById('stt-model-select')?.addEventListener('change', () => queuePersist('providers'));
  document.getElementById('llm-model-select')?.addEventListener('change', () => queuePersist('providers'));

  // Initial gate state once the form exists.
  updateProviderGates('stt');
  updateProviderGates('llm');
}

function selectProviderCard(type, preset) {
  document.querySelectorAll(`#${type}-provider-grid .provider-card`).forEach(c => {
    const isSelected = c.dataset.provider === preset;
    c.classList.toggle('selected', isSelected);
    c.setAttribute('aria-pressed', String(isSelected));
  });
}

async function fetchModels(type, silent = false) {
  const baseUrl = document.getElementById(`${type}-base-url`)?.value?.trim();
  const keyInput = document.getElementById(`${type}-api-key`)?.value?.trim();

  // Try stored key if input is empty
  let apiKey = keyInput;
  if (!apiKey) {
    const preset = document.querySelector(`#${type}-provider-grid .provider-card.selected`)?.dataset.provider || 'groq';
    const baseTarget = type === 'stt' ? 'Fluence/STT_ApiKey' : 'Fluence/LLM_ApiKey';
    const target = `${baseTarget}/${preset.toLowerCase().replace(/ /g, '_')}`;
    apiKey = await invoke('get_api_key', { target }).catch(() => '');
  }

  if (!baseUrl || !apiKey || apiKey.length < 8) {
    if (!silent) showToast('Please enter endpoint and API key first', 'error');
    return;
  }

  const btn = document.getElementById(`${type}-fetch-models-btn`);
  if (btn) btn.classList.add('animate-spin');

  try {
    // STT picker lists speech models only (backend filters out chat/LLM
    // ids); the LLM picker lists everything via fetch_models, unchanged.
    const select = document.getElementById(`${type}-model-select`);
    const current = select?.value || '';
    let models;
    let filtered = true;
    if (type === 'stt') {
      const res = await invoke('fetch_stt_models', { baseUrl, apiKey, keep: current || null });
      models = res.models || [];
      filtered = res.filtered !== false;
    } else {
      models = await invoke('fetch_models', { baseUrl, apiKey });
    }
    if (!models.length) {
      // Never strand the user: keep existing options when the endpoint
      // offers nothing (selectable).
      if (!silent) showToast('No models found on this endpoint - keeping current list', 'error');
      return;
    }
    if (select) {
      select.textContent = '';
      models.forEach(m => {
        const opt = document.createElement('option');
        opt.value = m;
        opt.textContent = m;
        if (m === current) opt.selected = true;
        select.appendChild(opt);
      });
    }
    if (!silent) {
      showToast(
        type === 'stt' && !filtered
          ? `Loaded ${models.length} models (unrecognized endpoint - showing all) ✓`
          : `Loaded ${models.length} models ✓`,
        'success'
      );
    }
  } catch (err) {
    const msg = 'Failed to fetch models: ' + err;
    if (!silent) showToast(msg, 'error');
    // Silent auto-fetch must still explain itself in-status — never fail quiet.
    const statusDot = document.querySelector(`#${type}-status .dot`);
    const statusText = document.getElementById(`${type}-status-text`);
    if (statusDot) statusDot.className = 'dot dot-error';
    if (statusText) statusText.textContent = String(err).replace('Error: ', '').slice(0, 120) || msg.slice(0, 120);
  } finally {
    if (btn) btn.classList.remove('animate-spin');
  }
}

async function testConnection(type) {
  const statusDot = document.querySelector(`#${type}-status .dot`);
  const statusText = document.getElementById(`${type}-status-text`);
  const { urlOk, keyOk, baseUrl } = providerReady(type);
  if (!urlOk || !keyOk) {
    if (statusDot) statusDot.className = 'dot dot-idle';
    if (statusText) statusText.textContent = 'Enter a valid URL and key (8+ chars), then test';
    return;
  }
  const preset = document.querySelector(`#${type}-provider-grid .provider-card.selected`)?.dataset.provider || 'groq';
  const baseTarget = type === 'stt' ? 'Fluence/STT_ApiKey' : 'Fluence/LLM_ApiKey';
  const target = `${baseTarget}/${preset.toLowerCase().replace(/ /g, '_')}`;
  const apiKey = await invoke('get_api_key', { target }).catch(() => '');
  const model = document.getElementById(`${type}-model-select`)?.value || '';

  if (statusDot) { statusDot.className = 'dot dot-idle'; }
  if (statusText) statusText.textContent = 'Testing…';

  try {
    let msg;
    if (type === 'stt') {
      msg = await invoke('test_stt_connection', { baseUrl, apiKey });
    } else {
      msg = await invoke('test_llm_connection', { baseUrl, apiKey, model });
    }
    if (statusDot) statusDot.className = 'dot dot-success';
    if (statusText) statusText.textContent = msg;
  } catch (err) {
    if (statusDot) statusDot.className = 'dot dot-error';
    if (statusText) statusText.textContent = String(err).replace('Error: ', '');
  }
}

// ── History ──────────────────────────────────────────────────────

function setupHistory() {
  let searchTimeout;
  document.getElementById('history-search')?.addEventListener('input', (e) => {
    clearTimeout(searchTimeout);
    searchTimeout = setTimeout(() => loadHistory(true, e.target.value), 300);
  });

  let historyLoadingMore = false;
  document.getElementById('load-more-btn')?.addEventListener('click', async () => {
    if (historyLoadingMore) return;
    const btn = document.getElementById('load-more-btn');
    historyLoadingMore = true;
    if (btn) { btn.disabled = true; btn.textContent = 'Loading…'; }
    historyPage++;
    try {
      const loaded = await loadHistory(false, document.getElementById('history-search')?.value);
      // A failed fetch must not consume the page, or the next retry
      // would silently skip a page of results.
      if (loaded === null) historyPage = Math.max(0, historyPage - 1);
    } finally {
      historyLoadingMore = false;
      if (btn) { btn.disabled = false; btn.textContent = 'Load More'; }
    }
  });

  document.getElementById('history-clear-search-btn')?.addEventListener('click', () => {
    const searchInput = document.getElementById('history-search');
    if (searchInput) searchInput.value = '';
    loadHistory(true, '');
  });

  document.getElementById('clear-history-btn')?.addEventListener('click', async () => {
    if (!confirm('Clear all transcription history?')) return;
    try {
      await invoke('clear_history');
      showToast('History cleared', 'success');
      loadHistory(true);
    } catch (err) {
      showToast('Failed to clear history: ' + err, 'error');
    }
  });

  // Right-click context menu for history rows (Copy / Delete)
  const historyList = document.getElementById('history-list');
  let historyMenuEl = null;
  let historyMenuRow = null;

  const hideHistoryMenu = () => {
    historyMenuEl?.remove();
    historyMenuEl = null;
    historyMenuRow = null;
  };

  document.addEventListener('click', hideHistoryMenu);
  document.addEventListener('keydown', (e) => {
    if (e.key === 'Escape' && historyMenuEl) {
      e.stopImmediatePropagation();
      const row = historyMenuRow;
      hideHistoryMenu();
      row?.focus();
    }
  });
  historyList?.addEventListener('scroll', hideHistoryMenu);
  document.querySelector('.content-area')?.addEventListener('scroll', hideHistoryMenu);

  const openHistoryMenu = (row, x, y) => {
    hideHistoryMenu();
    historyMenuRow = row;

    historyMenuEl = document.createElement('div');
    historyMenuEl.className = 'history-context-menu';
    historyMenuEl.setAttribute('role', 'menu');
    historyMenuEl.innerHTML = `
      <button type="button" role="menuitem" data-action="copy">Copy</button>
      <button type="button" role="menuitem" data-action="delete">Delete</button>
    `;

    historyMenuEl.addEventListener('click', (ev) => {
      ev.stopPropagation();
      const action = ev.target.dataset.action;
      if (action === 'copy') {
        const text = row.querySelector('.history-item-text')?.textContent || '';
        copyHistoryItem(text, row);
      } else if (action === 'delete') {
        deleteHistoryItem(row.dataset.historyId);
      }
      hideHistoryMenu();
    });
    historyMenuEl.addEventListener('contextmenu', (ev) => ev.preventDefault());
    historyMenuEl.addEventListener('keydown', (ev) => {
      if (ev.key === 'Escape') {
        hideHistoryMenu();
        row.focus();
      }
    });

    document.body.appendChild(historyMenuEl);

    const menuRect = historyMenuEl.getBoundingClientRect();
    x = Math.min(x, window.innerWidth - menuRect.width - 8);
    y = Math.min(y, window.innerHeight - menuRect.height - 8);
    historyMenuEl.style.left = x + 'px';
    historyMenuEl.style.top = y + 'px';

    historyMenuEl.querySelector('button')?.focus();
  };

  // Candidate word markers: click (or Enter/Space) accepts the suggestion
  const acceptMarkedWord = async (mark) => {
    const id = mark?.dataset?.suggestionId;
    if (!id) return;
    await acceptSuggestion(id);
    await loadPendingSuggestionMap(true);
    loadHistory(true, document.getElementById('history-search')?.value);
  };

  historyList?.addEventListener('click', (e) => {
    const mark = e.target.closest('.candidate-word');
    if (!mark) return;
    e.stopPropagation();
    acceptMarkedWord(mark);
  });

  historyList?.addEventListener('keydown', (e) => {
    if (e.key === 'F10' && e.shiftKey) {
      const row = e.target.closest?.('.history-item') || document.activeElement?.closest?.('.history-item');
      if (!row) return;
      e.preventDefault();
      const rect = row.getBoundingClientRect();
      openHistoryMenu(row, rect.left + 8, rect.top);
      return;
    }
    if (e.key !== 'Enter' && e.key !== ' ') return;
    const mark = e.target.closest('.candidate-word');
    if (!mark) return;
    e.preventDefault();
    e.stopPropagation();
    acceptMarkedWord(mark);
  });

  historyList?.addEventListener('contextmenu', (e) => {
    const row = e.target.closest('.history-item');
    if (!row) return;
    e.preventDefault();
    openHistoryMenu(row, e.clientX, e.clientY);
  });
}

async function loadHistory(reset, search = '') {
  if (reset) historyPage = 0;
  historySearchQuery = search || document.getElementById('history-search')?.value || '';

  try {
    await loadPendingSuggestionMap();
    const entries = await invoke('get_history', {
      page: historyPage,
      searchQuery: search || null,
    });

    const list = document.getElementById('history-list');
    const emptyEl = document.getElementById('history-empty');
    const noResultsEl = document.getElementById('history-no-results');

    if (reset && list) {
      list.querySelectorAll('.history-item, .history-group-header').forEach(el => el.remove());
      historyGroupKey = null;
    }

    if (entries.length === 0 && historyPage === 0) {
      if (historySearchQuery) {
        if (emptyEl) emptyEl.style.display = 'none';
        if (noResultsEl) {
          noResultsEl.classList.remove('hidden');
          setText('history-no-results-hint', 'Nothing matches "' + historySearchQuery + '" on this device.');
        }
      } else {
        if (noResultsEl) noResultsEl.classList.add('hidden');
        if (emptyEl) emptyEl.style.display = '';
      }
    } else {
      if (emptyEl) emptyEl.style.display = 'none';
      if (noResultsEl) noResultsEl.classList.add('hidden');
      entries.forEach(entry => renderHistoryItem(entry, list));
      const distinctDays = new Set();
      list?.querySelectorAll('.history-group-header')?.forEach(h => {
        if (h.dataset.dayKey) distinctDays.add(h.dataset.dayKey);
      });
      list?.classList.toggle('single-day', distinctDays.size <= 1);
      updateHistoryGroupCounts(list);
    }

    const loadMore = document.getElementById('history-load-more');
    if (loadMore) loadMore.classList.toggle('hidden', entries.length < 50);
    return entries.length;
  } catch (err) {
    showToast('Failed to load history: ' + err, 'error');
    return null;
  }
}

let lastDayCounts = [0, 0, 0, 0, 0, 0, 0];
let lastWeekStartMs = 0;
let chartResizeObs = null;

function renderWeeklyAreaChart(dayCounts, weekStartMs) {
  const svg = document.getElementById('weekly-area-svg');
  const areaPath = document.getElementById('weekly-area-path');
  const curvePath = document.getElementById('weekly-curve-path');
  const pointsGroup = document.getElementById('weekly-chart-points');
  const labelsGroup = document.getElementById('weekly-chart-labels');
  const gridGroup = document.getElementById('weekly-chart-grid');
  if (!svg || !areaPath || !curvePath || !pointsGroup) return;

  lastDayCounts = Array.isArray(dayCounts) && dayCounts.length === 7 ? dayCounts.slice() : [0, 0, 0, 0, 0, 0, 0];
  if (weekStartMs) lastWeekStartMs = weekStartMs;

  // Measure the real canvas width so the curve never stretches.
  // Falls back to a sensible default before first layout.
  const canvasCol = document.getElementById('chart-canvas-col');
  const measuredW = canvasCol ? canvasCol.clientWidth : 0;
  const width = Math.max(280, Math.min(1400, measuredW || 640));
  const height = 244;
  const paddingX = 14;
  const baselineY = height - 14;
  const topY = 20;
  const availHeight = baselineY - topY;
  const stepX = (width - paddingX * 2) / 6;

  svg.setAttribute('viewBox', '0 0 ' + width + ' ' + height);
  svg.removeAttribute('preserveAspectRatio');
  const grad = document.querySelector('#curve-stroke-grad');
  if (grad) {
    grad.setAttribute('gradientUnits', 'userSpaceOnUse');
    grad.setAttribute('x1', '0');
    grad.setAttribute('y1', '0');
    grad.setAttribute('x2', String(width));
    grad.setAttribute('y2', '0');
  }

  const maxCount = Math.max.apply(null, lastDayCounts.concat([1]));
  const points = [];

  for (let i = 0; i < 7; i++) {
    const x = paddingX + i * stepX;
    const factor = Math.min(1, Math.max(0, lastDayCounts[i] / maxCount));
    const y = lastDayCounts[i] > 0 ? baselineY - factor * availHeight : baselineY;
    points.push({ x: x, y: y, count: lastDayCounts[i] });
  }

  // Grid lines track the same geometry (no stretched static lines)
  if (gridGroup) {
    while (gridGroup.firstChild) gridGroup.removeChild(gridGroup.firstChild);
    const ns = 'http://www.w3.org/2000/svg';
    const rows = [
      { y: topY, strong: false },
      { y: topY + availHeight / 2, strong: false },
      { y: baselineY, strong: true }
    ];
    rows.forEach(function (row) {
      const line = document.createElementNS(ns, 'line');
      line.setAttribute('x1', paddingX);
      line.setAttribute('x2', width - paddingX);
      line.setAttribute('y1', row.y.toFixed(1));
      line.setAttribute('y2', row.y.toFixed(1));
      line.setAttribute('stroke', row.strong ? 'rgba(255,255,255,0.14)' : 'rgba(255,255,255,0.06)');
      line.setAttribute('stroke-width', '1');
      if (!row.strong) line.setAttribute('stroke-dasharray', '3 3');
      gridGroup.appendChild(line);
    });
  }

  // Smooth spline through points (Catmull-Rom to Bezier)
  let dCurve = 'M ' + points[0].x.toFixed(1) + ' ' + points[0].y.toFixed(1);
  for (let i = 0; i < points.length - 1; i++) {
    const p0 = points[i === 0 ? 0 : i - 1];
    const p1 = points[i];
    const p2 = points[i + 1];
    const p3 = points[i + 2 < points.length ? i + 2 : i + 1];

    const cp1x = p1.x + (p2.x - p0.x) / 6;
    const cp1y = Math.min(baselineY, Math.max(topY - 6, p1.y + (p2.y - p0.y) / 6));
    const cp2x = p2.x - (p3.x - p1.x) / 6;
    const cp2y = Math.min(baselineY, Math.max(topY - 6, p2.y - (p3.y - p1.y) / 6));

    dCurve += ' C ' + cp1x.toFixed(1) + ' ' + cp1y.toFixed(1) + ', ' + cp2x.toFixed(1) + ' ' + cp2y.toFixed(1) + ', ' + p2.x.toFixed(1) + ' ' + p2.y.toFixed(1);
  }

  const dArea = dCurve + ' L ' + points[6].x.toFixed(1) + ' ' + baselineY + ' L ' + points[0].x.toFixed(1) + ' ' + baselineY + ' Z';

  curvePath.setAttribute('d', dCurve);
  areaPath.setAttribute('d', dArea);

  // Dots + numeric labels above each dot
  while (pointsGroup.firstChild) {
    pointsGroup.removeChild(pointsGroup.firstChild);
  }
  if (labelsGroup) {
    while (labelsGroup.firstChild) labelsGroup.removeChild(labelsGroup.firstChild);
  }

  const ns = 'http://www.w3.org/2000/svg';
  const colors = ['#0BD6E3', '#0BD6E3', '#0BD6E3', '#0BD6E3', '#0BD6E3', '#0BD6E3', '#0BD6E3'];
  const dayNames = ['Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat', 'Sun'];
  let peakIdx = 0;
  lastDayCounts.forEach(function (c, i) { if (c > lastDayCounts[peakIdx]) peakIdx = i; });
  const total = lastDayCounts.reduce(function (a, b) { return a + b; }, 0);
  const reduced = window.matchMedia && window.matchMedia('(prefers-reduced-motion: reduce)').matches;

  const tooltip = document.getElementById('chart-tooltip');
  const showTip = function (idx, cx, cy) {
    if (!tooltip || !canvasCol) return;
    const pct = (cx / width) * 100;
    tooltip.hidden = false;
    tooltip.innerHTML = '<strong>' + lastDayCounts[idx] + '</strong> · ' + dayNames[idx];
    tooltip.style.left = pct + '%';
    tooltip.style.top = (cy - 6) + 'px';
  };
  const hideTip = function () { if (tooltip) tooltip.hidden = true; };

  points.forEach(function (pt, idx) {
    const isPeak = total > 0 && pt.count === maxCount && pt.count > 0;

    const halo = document.createElementNS(ns, 'circle');
    halo.setAttribute('cx', pt.x.toFixed(1));
    halo.setAttribute('cy', pt.y.toFixed(1));
    halo.setAttribute('r', pt.count > 0 ? (isPeak ? '7' : '6') : '3');
    halo.setAttribute('fill', colors[idx]);
    halo.setAttribute('opacity', pt.count > 0 ? '0.22' : '0.05');
    pointsGroup.appendChild(halo);

    const dot = document.createElementNS(ns, 'circle');
    dot.setAttribute('cx', pt.x.toFixed(1));
    dot.setAttribute('cy', pt.y.toFixed(1));
    dot.setAttribute('r', pt.count > 0 ? (isPeak ? '4' : '3.4') : '2.2');
    dot.setAttribute('fill', isPeak ? '#FFFFFF' : '#0D0D0D');
    dot.setAttribute('stroke', colors[idx]);
    dot.setAttribute('stroke-width', '2');
    dot.classList.add('chart-data-dot');
    dot.setAttribute('tabindex', '0');
    dot.setAttribute('role', 'img');
    dot.setAttribute('aria-label', dayNames[idx] + ': ' + pt.count + ' transcriptions');
    dot.addEventListener('mouseenter', function () { showTip(idx, pt.x, pt.y); });
    dot.addEventListener('mouseleave', hideTip);
    dot.addEventListener('focus', function () { showTip(idx, pt.x, pt.y); });
    dot.addEventListener('blur', hideTip);
    pointsGroup.appendChild(dot);

    if (labelsGroup && pt.count > 0) {
      const label = document.createElementNS(ns, 'text');
      label.setAttribute('x', pt.x.toFixed(1));
      label.setAttribute('y', Math.max(10, pt.y - 12).toFixed(1));
      label.setAttribute('class', 'chart-value-label' + (isPeak ? ' is-peak' : ''));
      label.textContent = String(pt.count);
      labelsGroup.appendChild(label);
    }
  });

  if (!reduced) {
    curvePath.style.opacity = '0';
    areaPath.style.opacity = '0';
    requestAnimationFrame(function () {
      curvePath.style.transition = 'opacity 0.35s ease';
      areaPath.style.transition = 'opacity 0.45s ease';
      curvePath.style.opacity = '1';
      areaPath.style.opacity = '1';
    });
  } else {
    curvePath.style.opacity = '1';
    areaPath.style.opacity = '1';
  }

  // Weekday footer states: today + peak
  let todayIdx = -1;
  if (lastWeekStartMs) {
    const diff = Date.now() - lastWeekStartMs;
    todayIdx = Math.floor(diff / 86400000);
    if (todayIdx < 0 || todayIdx > 6) todayIdx = -1;
  }
  document.querySelectorAll('#chart-day-labels .chart-day-cell').forEach(function (cell) {
    const i = Number(cell.getAttribute('data-day'));
    cell.classList.toggle('is-today', i === todayIdx);
    cell.classList.toggle('is-peak', total > 0 && i === peakIdx && lastDayCounts[i] > 0);
  });

  // Week insights strip — pure derivation from the same 7 counts already
  // in memory. No new IPC, no DB access: Android can reuse this 1:1 later.
  const activeDays = lastDayCounts.filter(function (c) { return c > 0; }).length;
  const avg = total > 0 ? total / 7 : 0;
  setText('insight-best-day', total > 0 ? dayNames[peakIdx] : '–');
  setText('insight-best-day-sub', total > 0 ? lastDayCounts[peakIdx] + ' transcriptions' : 'no activity yet');
  setText('insight-daily-avg', total > 0 ? (avg >= 10 ? String(Math.round(avg)) : avg.toFixed(1)) : '0');
  setText('insight-active-days', String(activeDays));
  setText('insight-active-sub', 'of 7 days' + (activeDays === 7 ? ' · perfect week' : ''));
  setText('insight-today', todayIdx >= 0 ? String(lastDayCounts[todayIdx]) : '–');
  setText('insight-today-sub', todayIdx >= 0 ? dayNames[todayIdx] + ' · so far' : 'outside this week');

  const emptyEl = document.getElementById('chart-empty');
  if (emptyEl) emptyEl.hidden = total !== 0;

  svg.setAttribute('aria-label', 'Weekly transcription activity: ' + total + ' transcriptions this week. ' +
    lastDayCounts.map(function (c, i) { return dayNames[i] + ' ' + c; }).join(', '));

  setupChartResize();
}

function setupChartResize() {
  if (chartResizeObs || !window.ResizeObserver) return;
  const canvasCol = document.getElementById('chart-canvas-col');
  if (!canvasCol) return;
  let t = null;
  chartResizeObs = new ResizeObserver(function () {
    if (t) clearTimeout(t);
    t = setTimeout(function () {
      // Re-render with identical data at the new width — no backend call.
      renderWeeklyAreaChart(lastDayCounts, lastWeekStartMs);
    }, 120);
  });
  chartResizeObs.observe(canvasCol);
}

async function loadDashboardStats() {
  try {
    // Account-level statistics: when signed in, totals come from the merged
    // account ledger (Windows + Android contributions combined). Signed out
    // or before the first sync, the backend falls back to platform-local
    // history-derived numbers (source: "local").
    const bStats = await invoke('get_account_stats');
    const totalWords = bStats.total_words;
    const weeklyWords = bStats.weekly_words;
    const weeklyDurationMs = bStats.weekly_duration_ms;
    const monthlyWords = bStats.monthly_words;

    if (totalWords >= 1000000) {
      animateStatValue('stat-total-words', (totalWords / 1000000).toFixed(1) + 'M');
    } else if (totalWords >= 1000) {
      animateStatValue('stat-total-words', (totalWords / 1000).toFixed(1) + 'K');
    } else {
      animateStatValue('stat-total-words', totalWords.toLocaleString());
    }

    // Converts a millisecond duration into a readable "Xh Ym" / "Ym" label.
    function formatDurationMs(ms) {
      if (!(ms > 0)) {
        return '0m';
      }
      const totalMinutes = Math.round(ms / 60000);
      const hours = Math.floor(totalMinutes / 60);
      const minutes = totalMinutes % 60;
      if (hours > 0) {
        return minutes > 0 ? `${hours}h ${minutes}m` : `${hours}h`;
      }
      return `${minutes}m`;
    }

    // Time saved from backend stats (estimated typing time: ~40 WPM average)
    animateStatValue('stat-time-saved', formatDurationMs((totalWords / 40) * 60000));

    // Dictation time (actual recording duration)
    animateStatValue('stat-dictation-time', formatDurationMs(bStats.total_duration_ms));

    const monthlySavedMs = (monthlyWords / 40 / 60) * 3600000;
    animateStatValue('stat-monthly-saved', formatDurationMs(monthlySavedMs));

    // Weekly activity bar chart - UTC Monday boundary comes from the backend
    // (identical across devices/timezones and matches the weekly header).
    const monday = new Date(bStats.week_start_ms);

    // Account mode returns weekly timestamps directly from the merged event
    // ledger; local mode falls back to querying transcription history.
    let timestamps;
    if (bStats.source === 'account' && Array.isArray(bStats.weekly_timestamps)) {
      timestamps = bStats.weekly_timestamps;
    } else {
      timestamps = await invoke('get_weekly_activity', { startOfWeekUtc: monday.toISOString() });
    }

    const dayCounts = [0, 0, 0, 0, 0, 0, 0];
    timestamps.forEach(ts => {
      const d = new Date(ts);
      const dow = d.getUTCDay();
      dayCounts[dow === 0 ? 6 : dow - 1]++;
    });

    renderWeeklyAreaChart(dayCounts, bStats.week_start_ms);
    // Week range label, e.g. "Jun 2 – Jun 8"
    try {
      const start = new Date(bStats.week_start_ms);
      const end = new Date(bStats.week_start_ms + 6 * 86400000);
      const fmt = new Intl.DateTimeFormat('en-US', { month: 'short', day: 'numeric' });
      setText('chart-range-label', fmt.format(start) + ' – ' + fmt.format(end));
    } catch (_) { /* range label is decorative */ }
    // KPI sub-labels derived from the same payload — no extra backend calls.
    setText('stat-total-words-sub', '+' + (weeklyWords >= 1000 ? (weeklyWords / 1000).toFixed(1) + 'K' : String(weeklyWords)) + ' this week');
    setText('stat-time-saved-sub', 'at ~40 WPM · to date');
    const weeklyHoursSaved = (weeklyWords / 40 / 60);
    const weeklyDictationHours = (weeklyDurationMs / 3600000);
    const weeklyWordsLabel = weeklyWords >= 1000 ? (weeklyWords / 1000).toFixed(1) + 'K' : weeklyWords.toLocaleString();
    const weeklySavedLabel = weeklyHoursSaved >= 1 ? weeklyHoursSaved.toFixed(1) + 'h saved' : Math.round(weeklyHoursSaved * 60) + 'm saved';
    const weeklyDictLabel = weeklyDictationHours >= 1 ? weeklyDictationHours.toFixed(1) + 'h spoken' : Math.round(weeklyDictationHours * 60) + 'm spoken';
    setText('chart-header-count', weeklyWordsLabel + ' words · ' + weeklySavedLabel + ' · ' + weeklyDictLabel);
    setText('stat-dictation-sub', weeklyDictLabel + ' · last 7 days');
    setText('stat-monthly-sub', (monthlyWords >= 1000 ? (monthlyWords / 1000).toFixed(1) + 'K' : String(monthlyWords)) + ' words · last 30 days');
  } catch (err) {
    console.error('Failed to load dashboard stats:', err);
  }
}

window.deleteHistoryItem = async (id) => {
  try {
    await invoke('delete_history_entry', { id });
    document.querySelector(`[data-history-id="${id}"]`)?.remove();
    updateHistoryGroupCounts(document.getElementById('history-list'));
    showToast('Deleted', 'success');
  } catch (err) {
    showToast('Failed to delete: ' + err, 'error');
  }
};

// ── Auto-Apply Settings ─────────────────────────────────────────
// Every settings change persists immediately (debounced). The Save
// buttons remain only as an explicit flush; nothing is ever lost by
// closing the window without clicking Save.

let persistTimer = null;
let persistFeatures = new Set();
let lastAppliedHotkeys = null;
let lastAppliedAutostart = null;

const GENERAL_BINDINGS = [
  { id: 'recording-mode-select',       key: 'recording_mode',        type: 'select',   features: ['hotkeys'] },
  { id: 'agent-recording-mode-select', key: 'agent_recording_mode',  type: 'select',   features: ['hotkeys'] },
  { id: 'overlay-position-select',     key: 'overlay_position',      type: 'select' },
  { id: 'overlay-style-select',        key: 'overlay_style',         type: 'select' },
  { id: 'language-select',             key: 'language',              type: 'select' },
  { id: 'ai-polish-select',            key: 'ai_polish_style',       type: 'select' },
  { id: 'audio-device-select',         key: 'audio_device_id',       type: 'select' },
  { id: 'autostart-cb',                key: 'auto_start',            type: 'checkbox', features: ['autostart'] },
  { id: 'duck-cb',                     key: 'duck_enabled',          type: 'checkbox' },
  { id: 'auto-grab-cb',                key: 'auto_grab_highlight',   type: 'checkbox' },
  { id: 'auto-learn-cb',               key: 'auto_learn_enabled',    type: 'checkbox' },
  { id: 'auto-accept-cb',              key: 'auto_accept_enabled',   type: 'checkbox' },
  { id: 'sound-on-complete-cb',        key: 'sound_on_complete',     type: 'checkbox' },
];

function setupAutoApply() {
  lastAppliedAutostart = currentSettings?.auto_start ?? false;

  GENERAL_BINDINGS.forEach(({ id, key, type, features = [] }) => {
    document.getElementById(id)?.addEventListener('change', (e) => {
      if (!currentSettings) return;
      currentSettings[key] = type === 'checkbox' ? e.target.checked : e.target.value;
      queuePersist('general', ...features);
    });
  });

  // Auto-accept depends on auto-learn: turning the master off forces the
  // dependent toggle off (persisted) and disables its input. Registered
  // after the generic binding so it runs after the generic state update.
  document.getElementById('auto-learn-cb')?.addEventListener('change', () => syncLearnAcceptUI());
}

// Auto-accept depends on auto-learn. When the master is off, the dependent
// toggle is forced off, persisted, and disabled with an explanatory title;
// when the master is on, the dependent toggle is re-enabled untouched.
function syncLearnAcceptUI() {
  const learnCb = document.getElementById('auto-learn-cb');
  const acceptCb = document.getElementById('auto-accept-cb');
  if (!learnCb || !acceptCb || !currentSettings) return;
  if (!learnCb.checked) {
    acceptCb.checked = false;
    acceptCb.disabled = true;
    acceptCb.title = 'Requires Auto-Learn';
    if (currentSettings.auto_accept_enabled !== false) {
      currentSettings.auto_accept_enabled = false;
      queuePersist('general');
    }
  } else {
    acceptCb.disabled = false;
    acceptCb.removeAttribute('title');
  }
}

function queuePersist(...features) {
  features.forEach(f => persistFeatures.add(f));
  clearTimeout(persistTimer);
  persistTimer = setTimeout(flushPendingPersists, 350);
  updateSaveButtons();
}

function updateSaveButtons() {
  const dirty = persistFeatures.size > 0;
  for (const id of ['save-general-btn', 'save-providers-btn']) {
    const btn = document.getElementById(id);
    if (!btn) continue;
    btn.classList.toggle('is-dirty', dirty);
    if (!btn.dataset.baseLabel) btn.dataset.baseLabel = btn.textContent.trim() || 'Save Changes';
    btn.textContent = dirty ? `● ${btn.dataset.baseLabel}` : btn.dataset.baseLabel;
  }
}

async function flushPendingPersists() {
  if (!currentSettings || persistFeatures.size === 0) {
    updateSaveButtons();
    return false;
  }
  const features = [...persistFeatures];
  persistFeatures.clear();

  if (features.includes('providers')) collectProviderSettings();

  try {
    await invoke('update_settings', { settings: currentSettings });
  } catch (err) {
    showToast('Failed to save settings: ' + err, 'error');
  }

  if (features.includes('hotkeys')) await applyHotkeyChanges();
  if (features.includes('autostart')) await applyAutostartChange();
  updateSaveButtons();
  return true;
}

async function applyHotkeyChanges() {
  const desired = {
    transcriptionShortcut: currentSettings.hotkey,
    transcriptionMode: currentSettings.recording_mode,
    agentShortcut: currentSettings.agent_hotkey,
    agentMode: currentSettings.agent_recording_mode,
  };
  if (lastAppliedHotkeys && JSON.stringify(lastAppliedHotkeys) === JSON.stringify(desired)) return;
  lastAppliedHotkeys = desired;
  try {
    await invoke('update_hotkeys', desired);
  } catch (err) {
    showToast('Failed to update hotkeys: ' + err, 'error');
  }
}

async function applyAutostartChange() {
  if (lastAppliedAutostart === currentSettings.auto_start) return;
  lastAppliedAutostart = currentSettings.auto_start;
  try {
    await invoke('set_autostart', { enabled: currentSettings.auto_start });
  } catch (err) {
    console.error('Failed to apply autostart:', err);
  }
}

function collectProviderSettings() {
  const sttPreset = document.querySelector('#stt-provider-grid .provider-card.selected')?.dataset.provider || 'groq';
  const llmPreset = document.querySelector('#llm-provider-grid .provider-card.selected')?.dataset.provider || 'groq';

  currentSettings.stt_provider = {
    preset: sttPreset,
    base_url: document.getElementById('stt-base-url')?.value?.trim() || '',
    model: document.getElementById('stt-model-select')?.value || '',
    api_key_saved: true,
  };

  currentSettings.llm_provider = {
    preset: llmPreset,
    base_url: document.getElementById('llm-base-url')?.value?.trim() || '',
    model: document.getElementById('llm-model-select')?.value || '',
    api_key_saved: true,
  };
}

// ── Save Buttons ─────────────────────────────────────────────────

function setupSaveButtons() {
  document.getElementById('save-general-btn')?.addEventListener('click', async () => {
    const flushed = await flushPendingPersists();
    showToast(flushed ? 'All changes saved' : 'Already up to date', flushed ? 'success' : 'info');
  });
  document.getElementById('save-providers-btn')?.addEventListener('click', async () => {
    const flushed = await flushPendingPersists();
    showToast(flushed ? 'All changes saved' : 'Already up to date', flushed ? 'success' : 'info');
  });
}

// Backwards-compatible aliases used by Ctrl+S shortcuts
const saveGeneral = () => flushPendingPersists();
const saveProviders = () => flushPendingPersists();

// ── Audio Devices ────────────────────────────────────────────────

async function populateAudioDevices() {
  try {
    const devices = await invoke('list_audio_devices');
    const select = document.getElementById('audio-device-select');
    if (!select) return;

    devices.forEach(name => {
      const opt = document.createElement('option');
      opt.value = name;
      opt.textContent = name;
      select.appendChild(opt);
    });

    if (currentSettings?.audio_device_id) {
      select.value = currentSettings.audio_device_id;
    }
  } catch {
    // Audio devices unavailable - silently fail
  }
}

// ── Tauri Events ─────────────────────────────────────────────────

async function listenForTauriEvents() {
  await listen('set-recording-mode', async (evt) => {
    const mode = evt.payload;
    setSelectValue('recording-mode-select', mode);
    if (currentSettings) {
      currentSettings.recording_mode = mode;
      // Persist the tray-selected mode so it survives app restarts
      try {
        await invoke('update_settings', { settings: currentSettings });
      } catch (err) {
        console.error('Failed to persist tray recording mode:', err);
      }
    }
  });

  await listen('history-updated', () => {
    if (currentPage === 'dashboard') {
      loadDashboardStats();
    } else if (currentPage === 'history') {
      loadHistory(true);
    }
  });

  // The main window is hidden, never destroyed — without this it reopens
  // wherever it was left (e.g. About). Reset to dashboard on every show,
  // unless a tray navigate-to (e.g. History) lands right after.
  await listen('window-visibility', (evt) => {
    if (evt.payload !== true) return;
    setTimeout(() => {
      if (Date.now() - lastTrayNavigateAt < 500) return;
      navigateTo('dashboard');
    }, 60);
  });
}

// ── App Version ──────────────────────────────────────────────────

let currentAppVersion = '1.15.0';

async function loadAppVersion() {
  try {
    const version = await invoke('get_app_version');
    currentAppVersion = version;
    setText('sidebar-version-label', `v${version}`);
    setText('about-version', version);
  } catch {}
}

// ── Toast Notifications ──────────────────────────────────────────

function showToast(message, type = 'info') {
  const container = document.getElementById('toast-container');
  if (!container) return;

  // Cap the stack so rapid saves can't pile toasts over content.
  while (container.children.length >= 3) container.firstChild.remove();

  const icons = { success: '✓', error: '✕', info: 'ℹ' };
  const colors = {
    success: 'var(--color-success)',
    error:   'var(--color-error)',
    info:    'var(--color-on-surface-variant)'
  };

  const toast = document.createElement('div');
  toast.className = `toast ${type}`;
  toast.innerHTML = `
    <span style="color:${colors[type]};font-weight:700;">${icons[type]}</span>
    <span>${escapeHtml(message)}</span>
  `;
  container.appendChild(toast);

  // Hover pauses dismissal so long messages stay readable.
  let dismissTimer = null;
  const dismiss = () => {
    toast.style.animation = 'fade-out 0.3s ease forwards';
    setTimeout(() => toast.remove(), 300);
  };
  const arm = (ms) => {
    clearTimeout(dismissTimer);
    dismissTimer = setTimeout(dismiss, ms);
  };
  toast.addEventListener('mouseenter', () => clearTimeout(dismissTimer));
  toast.addEventListener('mouseleave', () => arm(800));
  arm(3000);
}

// ── DOM Helpers ──────────────────────────────────────────────────

function setText(id, text) {
  const el = document.getElementById(id);
  if (el) el.textContent = text;
}

function setInputValue(id, value) {
  const el = document.getElementById(id);
  if (el) el.value = value;
}

function setSelectValue(id, value) {
  const el = document.getElementById(id);
  if (el) el.value = value;
}

function setSelectOption(id, value) {
  const el = document.getElementById(id);
  if (!el) return;
  // Add option if not present
  if (!Array.from(el.options).find(o => o.value === value)) {
    const opt = document.createElement('option');
    opt.value = value;
    opt.textContent = value;
    el.appendChild(opt);
  }
  el.value = value;
}

function setChecked(id, checked) {
  const el = document.getElementById(id);
  if (el) el.checked = checked;
}

function escapeHtml(str) {
  return String(str)
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;');
}

const statAnimFrames = new Map();

function animateStatValue(id, finalText) {
  const el = document.getElementById(id);
  if (!el || el.textContent === finalText) return;

  const reduced = window.matchMedia?.('(prefers-reduced-motion: reduce)').matches;
  const match = String(finalText).match(/^([\d.,]+)\s*(.*)$/);
  if (reduced || !match) {
    el.textContent = finalText;
    return;
  }

  const target = parseFloat(match[1].replace(/,/g, ''));
  const suffix = match[2];
  const decimals = match[1].includes('.') ? match[1].split('.')[1].length : 0;
  if (target === 0) {
    el.textContent = finalText;
    return;
  }

  const frame = statAnimFrames.get(id);
  if (frame) cancelAnimationFrame(frame);

  const duration = 700;
  const start = performance.now();
  const step = (now) => {
    const t = Math.min(1, (now - start) / duration);
    const eased = 1 - Math.pow(1 - t, 3);
    const value = target * eased;
    el.textContent = (decimals > 0 ? value.toFixed(decimals) : Math.round(value).toLocaleString()) + suffix;
    if (t < 1) {
      statAnimFrames.set(id, requestAnimationFrame(step));
    } else {
      el.textContent = finalText;
      statAnimFrames.delete(id);
    }
  };
  statAnimFrames.set(id, requestAnimationFrame(step));
}

// ── Offline ASR Downloader ────────────────────────────────────────

function updateSttUiVisibility(preset) {
  const credentialsWrapper = document.getElementById('stt-credentials-wrapper');
  const offlineDownloader = document.getElementById('stt-offline-downloader');
  
  if (preset === 'Local Offline') {
    if (credentialsWrapper) credentialsWrapper.classList.add('hidden');
    if (offlineDownloader) {
      offlineDownloader.classList.remove('hidden');
      updateOfflineStatus();
    }
  } else {
    if (credentialsWrapper) credentialsWrapper.classList.remove('hidden');
    if (offlineDownloader) offlineDownloader.classList.add('hidden');
  }
}

function currentSttPreset() {
  return document.querySelector('#stt-provider-grid .provider-card.selected')?.dataset.provider || 'groq';
}

// ── Offline engine cards (Android parity) ────────────────────────────
// One card per engine: click (or Enter/Space) selects it and persists via
// the standard auto-apply pipeline. All cards stay visible; each shows its
// own Download/Installed state.
const OFFLINE_ENGINE_CARDS = [
  { engine: 'moonshine_v2_small', cardId: 'v2small-model-card', statusCmd: 'get_moonshine_v2_small_model_status', downloadBtnId: 'v2small-download-btn', deleteBtnId: 'v2small-delete-btn' },
  { engine: 'sensevoice', cardId: 'sensevoice-model-card', statusCmd: 'get_offline_model_status', downloadBtnId: 'offline-download-btn', deleteBtnId: 'offline-delete-btn' },
  { engine: 'moonshine_v2_medium', cardId: 'v2medium-model-card', statusCmd: 'get_moonshine_v2_medium_model_status', downloadBtnId: 'v2medium-download-btn', deleteBtnId: 'v2medium-delete-btn' },
];

function getSelectedOfflineEngine() {
  return document.querySelector('#stt-offline-downloader .offline-model-card.selected')?.dataset.engine || 'sensevoice';
}

function selectOfflineEngineCard(engine, { persist = false } = {}) {
  const known = OFFLINE_ENGINE_CARDS.some(c => c.engine === engine);
  const target = known ? engine : 'sensevoice';
  document.querySelectorAll('#stt-offline-downloader .offline-model-card').forEach(card => {
    const selected = card.dataset.engine === target;
    card.classList.toggle('selected', selected);
    card.setAttribute('aria-checked', selected ? 'true' : 'false');
  });
  if (persist && typeof currentSettings !== 'undefined' && currentSettings) {
    currentSettings.offline_engine = target;
    queuePersist('general');
  }
  return target;
}

async function updateOfflineStatus() {
  const progressWrapper = document.getElementById('offline-progress-wrapper');

  for (const cfg of OFFLINE_ENGINE_CARDS) {
    try {
      const isInstalled = await invoke(cfg.statusCmd);
      const downloadBtn = document.getElementById(cfg.downloadBtnId);
      const deleteBtn = document.getElementById(cfg.deleteBtnId);

      if (isInstalled) {
        if (downloadBtn) { downloadBtn.textContent = 'Installed'; downloadBtn.disabled = true; }
        if (deleteBtn) deleteBtn.classList.remove('hidden');
        if (progressWrapper) progressWrapper.classList.add('hidden');
      } else {
        if (downloadBtn) { downloadBtn.textContent = 'Download Model'; downloadBtn.disabled = false; }
        if (deleteBtn) deleteBtn.classList.add('hidden');
      }
    } catch (err) {
      console.error('Failed to get offline model status:', err);
    }
  }
}

async function setupOfflineDownloader() {
  const downloadBtn = document.getElementById('offline-download-btn');
  const deleteBtn = document.getElementById('offline-delete-btn');
  const v2smallDownloadBtn = document.getElementById('v2small-download-btn');
  const v2smallDeleteBtn = document.getElementById('v2small-delete-btn');
  const v2mediumDownloadBtn = document.getElementById('v2medium-download-btn');
  const v2mediumDeleteBtn = document.getElementById('v2medium-delete-btn');
  const cancelBtn = document.getElementById('offline-cancel-btn');
  const progressWrapper = document.getElementById('offline-progress-wrapper');
  
  // Engine card selection (radio behaviour; persists via auto-apply).
  // Button clicks inside a card are ignored here - download/delete manage
  // themselves and must not switch the selected engine as a side effect.
  document.querySelectorAll('#stt-offline-downloader .offline-model-card').forEach(card => {
    card.addEventListener('click', (e) => {
      if (e.target.closest('button')) return;
      selectOfflineEngineCard(card.dataset.engine, { persist: true });
    });
    card.addEventListener('keydown', (e) => {
      if (e.target.closest('button')) return;
      if (e.key === 'Enter' || e.key === ' ') {
        e.preventDefault();
        selectOfflineEngineCard(card.dataset.engine, { persist: true });
      }
    });
  });

  // Fast (Multilingual) download button
  if (downloadBtn) {
    downloadBtn.addEventListener('click', async () => {
      try {
        downloadBtn.disabled = true;
        downloadBtn.textContent = 'Connecting…';
        if (progressWrapper) progressWrapper.classList.remove('hidden');
        await invoke('download_offline_model');
      } catch (err) {
        showToast('Failed to start download: ' + err, 'error');
        updateOfflineStatus();
      }
    });
  }

  // Fast (Multilingual) delete button
  if (deleteBtn) {
    deleteBtn.addEventListener('click', async () => {
      if (!confirm('Are you sure you want to delete the Fast (Multilingual) model files to free space (~239 MB)?')) return;
      try {
        const bytesFreed = await invoke('delete_offline_model');
        const mbFreed = (bytesFreed / (1024 * 1024)).toFixed(1);
        showToast(`Fast (Multilingual) model deleted. Freed ${mbFreed} MB ✓`, 'success');
        updateOfflineStatus();
      } catch (err) {
        showToast('Failed to delete model files: ' + err, 'error');
      }
    });
  }

  // Fast (English) download button
  if (v2smallDownloadBtn) {
    v2smallDownloadBtn.addEventListener('click', async () => {
      try {
        v2smallDownloadBtn.disabled = true;
        v2smallDownloadBtn.textContent = 'Connecting…';
        if (progressWrapper) progressWrapper.classList.remove('hidden');
        await invoke('download_moonshine_v2_small_model');
      } catch (err) {
        showToast('Failed to start download: ' + err, 'error');
        updateOfflineStatus();
      }
    });
  }

  // Fast (English) delete button
  if (v2smallDeleteBtn) {
    v2smallDeleteBtn.addEventListener('click', async () => {
      if (!confirm('Are you sure you want to delete the Fast (English) model files to free space (~142 MB)?')) return;
      try {
        const bytesFreed = await invoke('delete_moonshine_v2_small_model');
        const mbFreed = (bytesFreed / (1024 * 1024)).toFixed(1);
        showToast(`Fast (English) model deleted. Freed ${mbFreed} MB ✓`, 'success');
        updateOfflineStatus();
      } catch (err) {
        showToast('Failed to delete model files: ' + err, 'error');
      }
    });
  }

  // Pro (English) download button
  if (v2mediumDownloadBtn) {
    v2mediumDownloadBtn.addEventListener('click', async () => {
      try {
        v2mediumDownloadBtn.disabled = true;
        v2mediumDownloadBtn.textContent = 'Connecting…';
        if (progressWrapper) progressWrapper.classList.remove('hidden');
        await invoke('download_moonshine_v2_medium_model');
      } catch (err) {
        showToast('Failed to start download: ' + err, 'error');
        updateOfflineStatus();
      }
    });
  }

  // Pro (English) delete button
  if (v2mediumDeleteBtn) {
    v2mediumDeleteBtn.addEventListener('click', async () => {
      if (!confirm('Are you sure you want to delete the Pro (English) model files to free space (~269 MB)?')) return;
      try {
        const bytesFreed = await invoke('delete_moonshine_v2_medium_model');
        const mbFreed = (bytesFreed / (1024 * 1024)).toFixed(1);
        showToast(`Pro (English) model deleted. Freed ${mbFreed} MB ✓`, 'success');
        updateOfflineStatus();
      } catch (err) {
        showToast('Failed to delete model files: ' + err, 'error');
      }
    });
  }

  // Cancel button (shared)
  if (cancelBtn) {
    cancelBtn.addEventListener('click', async () => {
      try {
        await invoke('cancel_offline_download');
      } catch (err) {
        console.error('Failed to cancel download:', err);
      }
    });
  }

  // Listen to Tauri progress events
  if (window.__TAURI__) {
    const { listen } = window.__TAURI__.event;
    await listen('offline-download-progress', (event) => {
      const payload = event.payload;
      const statusText = document.getElementById('offline-progress-status');
      const percentageText = document.getElementById('offline-progress-percentage');
      const progressFill = document.getElementById('offline-progress-fill');
      const progressTrack = document.getElementById('offline-progress-track');
      const bytesText = document.getElementById('offline-progress-bytes');
      
      const progress = payload.progress;
      const status = payload.status;
      const currentFile = payload.currentFile;
      
      if (status === 'downloading') {
        if (statusText) statusText.textContent = `Downloading: ${currentFile}`;
        if (percentageText) percentageText.textContent = `${progress.toFixed(0)}%`;
        if (progressFill) progressFill.style.width = `${progress}%`;
        if (progressTrack) progressTrack.setAttribute('aria-valuenow', Math.round(progress));
        
        const downloadedMb = (payload.bytesDownloaded / (1024 * 1024)).toFixed(1);
        const totalMb = (payload.totalBytes / (1024 * 1024)).toFixed(1);
        if (bytesText) bytesText.textContent = `${downloadedMb} / ${totalMb} MB`;
      } else if (status === 'extracting') {
        if (statusText) statusText.textContent = 'Extracting model files…';
        if (percentageText) percentageText.textContent = `${progress.toFixed(0)}%`;
        if (progressFill) progressFill.style.width = `${progress}%`;
        if (progressTrack) progressTrack.setAttribute('aria-valuenow', Math.round(progress));
      } else if (status === 'completed') {
        showToast('Offline model downloaded and installed successfully ✓', 'success');
        updateOfflineStatus();
      } else if (status === 'error') {
        showToast('Offline download failed: ' + payload.errorMessage, 'error');
        updateOfflineStatus();
      } else if (status === 'cancelled') {
        showToast('Offline download cancelled', 'info');
        updateOfflineStatus();
      }
    });
  }
}

// ── Dictionary ───────────────────────────────────────────────────

function setupDictionary() {
  document.getElementById('add-dict-btn')?.addEventListener('click', () => {
    toggleDictAddRow(true);
  });

  document.getElementById('dict-cancel-btn')?.addEventListener('click', () => {
    toggleDictAddRow(false);
  });

  document.getElementById('dict-save-btn')?.addEventListener('click', saveDictEntry);

  document.getElementById('import-dict-btn')?.addEventListener('click', importDictionary);
  document.getElementById('export-dict-btn')?.addEventListener('click', exportDictionary);
}

function toggleDictAddRow(show) {
  const row = document.getElementById('dict-add-row');
  if (row) row.classList.toggle('hidden', !show);
  if (show) {
    document.getElementById('dict-spoken-input')?.focus();
  } else {
    setInputValue('dict-spoken-input', '');
    setInputValue('dict-corrected-input', '');
  }
}

let autoAddedKeys = new Set();

async function loadDictionary() {
  try {
    dictEntries = await invoke('get_dictionary');
    // Provenance for the "Added" badge: canonical pair keys of auto-added
    // rows (mirrors dictionary::canonical_entry_key). Fetched only in auto
    // mode; otherwise the set stays empty and no badges render.
    autoAddedKeys = new Set();
    if (currentSettings?.auto_accept_enabled === true) {
      try {
        const autoAdded = await invoke('get_auto_accepted_suggestions');
        autoAddedKeys = new Set(autoAdded.map(s =>
          s.spoken.trim().toLowerCase() + '\u0000' + s.corrected.trim().toLowerCase()));
      } catch (err) {
        console.error('Failed to load auto-added provenance:', err);
      }
    }
    renderDictTable();
  } catch (err) {
    showToast('Failed to load dictionary: ' + err, 'error');
  }
}

function renderDictTable() {
  const tbody = document.getElementById('dict-table-body');
  const emptyRow = document.getElementById('dict-empty-row');
  if (!tbody) return;

  // Remove all non-empty rows
  tbody.querySelectorAll('tr[data-dict-id]').forEach(r => r.remove());

  if (dictEntries.length === 0) {
    if (emptyRow) emptyRow.style.display = '';
  } else {
    if (emptyRow) emptyRow.style.display = 'none';
    dictEntries.forEach(entry => {
      const tr = document.createElement('tr');
      tr.dataset.dictId = entry.id;
      const autoKey = entry.spoken.trim().toLowerCase() + '\u0000' + entry.corrected.trim().toLowerCase();
      tr.innerHTML = `
        <td class="spoken-word">${escapeHtml(entry.spoken)}</td>
        <td class="corrected-word">${escapeHtml(entry.corrected)}</td>
        <td class="col-meta added-col">${autoAddedKeys.has(autoKey) ? '<span class="source-badge">auto</span>' : ''}</td>
        <td class="actions">
          <button class="btn-ghost btn-small dict-delete-btn" data-dict-id="${entry.id}">Delete</button>
        </td>
      `;
      tr.querySelector('.dict-delete-btn')?.addEventListener('click', () => deleteDictEntry(entry.id));
      tbody.appendChild(tr);
    });
  }
}

async function saveDictEntry() {
  const spoken = document.getElementById('dict-spoken-input')?.value?.trim();
  const corrected = document.getElementById('dict-corrected-input')?.value?.trim();

  if (!spoken || !corrected) {
    showToast('Please fill in both fields', 'error');
    return;
  }

  try {
    const entry = await invoke('add_dictionary_entry', { spoken, corrected });
    dictEntries.push(entry);
    renderDictTable();
    toggleDictAddRow(false);
    showToast('Entry added ✓', 'success');
  } catch (err) {
    showToast('Failed to add entry: ' + err, 'error');
  }
}

window.deleteDictEntry = async (id) => {
  try {
    await invoke('delete_dictionary_entry', { id });
    dictEntries = dictEntries.filter(e => e.id !== id);
    renderDictTable();
    loadSuggestions();  // Delete→dismiss linkage may dismiss a suggestion row
    showToast('Entry deleted', 'success');
  } catch (err) {
    showToast('Failed to delete: ' + err, 'error');
  }
};

async function importDictionary() {
  try {
    const dialog = window.__TAURI_PLUGIN_DIALOG__;
    const fs = window.__TAURI_PLUGIN_FS__;
    if (!dialog || !fs) {
      showToast('File dialog plugin not available', 'error');
      return;
    }
    const path = await dialog.open({ filters: [{ name: 'JSON', extensions: ['json'] }] });
    if (!path) return;
    const json = await fs.readTextFile(path);
    const count = await invoke('import_dictionary', { jsonData: json });
    showToast(`Imported ${count} entries ✓`, 'success');
    loadDictionary();
  } catch (err) {
    showToast('Import failed: ' + err, 'error');
  }
}

async function exportDictionary() {
  try {
    const json = await invoke('export_dictionary');
    const blob = new Blob([json], { type: 'application/json' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = 'fluence-dictionary.json';
    a.click();
    URL.revokeObjectURL(url);
  } catch (err) {
    showToast('Export failed: ' + err, 'error');
  }
}

// ── Snippets (Text Expansion) ───────────────────────────────────

let snippetEntries = [];

function setupSnippets() {
  document.getElementById('snippets-enabled-cb')?.addEventListener('change', async (e) => {
    try {
      await invoke('set_snippets_enabled', { enabled: e.target.checked });
      showToast(e.target.checked ? 'Text expansion enabled ✓' : 'Text expansion disabled', 'success');
    } catch (err) {
      showToast('Failed to update: ' + err, 'error');
      e.target.checked = !e.target.checked;
    }
  });

  document.getElementById('add-snippet-btn')?.addEventListener('click', () => toggleSnippetAddRow(true));
  document.getElementById('snippet-cancel-btn')?.addEventListener('click', () => toggleSnippetAddRow(false));
  document.getElementById('snippet-save-btn')?.addEventListener('click', saveSnippetEntry);
}

function toggleSnippetAddRow(show) {
  const row = document.getElementById('snippet-add-row');
  if (row) row.classList.toggle('hidden', !show);
  if (show) {
    document.getElementById('snippet-trigger-input')?.focus();
  } else {
    setInputValue('snippet-trigger-input', '');
    setInputValue('snippet-expansion-input', '');
  }
}

async function loadSnippets() {
  try {
    const store = await invoke('get_snippets');
    setChecked('snippets-enabled-cb', store.enabled);
    snippetEntries = store.snippets || [];
    renderSnippetsTable();
  } catch (err) {
    showToast('Failed to load snippets: ' + err, 'error');
  }
}

function renderSnippetsTable() {
  const tbody = document.getElementById('snippet-table-body');
  const emptyRow = document.getElementById('snippet-empty-row');
  if (!tbody) return;

  tbody.querySelectorAll('tr[data-snippet-id]').forEach(r => r.remove());

  if (snippetEntries.length === 0) {
    if (emptyRow) emptyRow.style.display = '';
  } else {
    if (emptyRow) emptyRow.style.display = 'none';
    snippetEntries.forEach(entry => {
      const tr = document.createElement('tr');
      tr.dataset.snippetId = entry.id;
      tr.innerHTML = `
        <td class="spoken-word">${escapeHtml(entry.trigger)}</td>
        <td class="corrected-word">${escapeHtml(entry.expansion)}</td>
        <td class="actions">
          <button class="btn-ghost snippet-delete-btn" data-snippet-id="${entry.id}" style="padding:4px 8px;font-size:12px;color:var(--color-error)">Delete</button>
        </td>
      `;
      tr.querySelector('.snippet-delete-btn')?.addEventListener('click', () => deleteSnippetEntry(entry.id));
      tbody.appendChild(tr);
    });
  }
}

async function saveSnippetEntry() {
  const trigger = document.getElementById('snippet-trigger-input')?.value?.trim();
  const expansion = document.getElementById('snippet-expansion-input')?.value?.trim();

  if (!trigger || !expansion) {
    showToast('Please fill in both fields', 'error');
    return;
  }

  try {
    const entry = await invoke('add_snippet', { trigger, expansion });
    snippetEntries.push(entry);
    renderSnippetsTable();
    toggleSnippetAddRow(false);
    showToast('Snippet added ✓', 'success');
  } catch (err) {
    showToast(String(err).replace(/^Error:\s*/, ''), 'error');
  }
}

window.deleteSnippetEntry = async (id) => {
  try {
    await invoke('delete_snippet', { id });
    snippetEntries = snippetEntries.filter(e => e.id !== id);
    renderSnippetsTable();
    showToast('Snippet deleted', 'success');
  } catch (err) {
    showToast('Failed to delete: ' + err, 'error');
  }
};

// ── Sync ─────────────────────────────────────────────────────────

let syncStatus = null;

async function loadSyncPage() {
  try {
    syncStatus = await invoke('sync_get_status');
    renderSyncStatus();
  } catch (err) {
    showToast('Failed to load sync status: ' + err, 'error');
  }
}

function setupSyncPage() {
  document.getElementById('sync-enabled-cb')?.addEventListener('change', async (e) => {
    try {
      await invoke('sync_toggle', { enabled: e.target.checked });
      showToast(e.target.checked ? 'Background sync enabled' : 'Background sync disabled', 'success');
      loadSyncPage();
    } catch (err) {
      e.target.checked = !e.target.checked;
      showToast('Failed to update sync: ' + String(err).replace(/^Error:\s*/, ''), 'error');
    }
  });

  document.getElementById('sync-sign-in-btn')?.addEventListener('click', async (e) => {
    const btn = e.currentTarget;
    btn.disabled = true;
    btn.textContent = 'Opening browser…';
    try {
      const status = await invoke('sync_sign_in');
      showToast('Signed in as ' + (status?.account_key || 'your Google account'), 'success');
      syncStatus = status;
      renderSyncStatus();
      // Signing in (or switching accounts) changes which account's rows are
      // visible: refresh the dictionary/snippets lists so the table no longer
      // shows the previously-active account's rows (which the delete guard
      // would otherwise block as "belongs to another account").
      loadDictionary();
      loadSnippets();
    } catch (err) {
      showToast('Sign-in failed: ' + String(err).replace(/^Error:\s*/, ''), 'error');
      if (!syncStatus) syncStatus = {};
      syncStatus.last_error = String(err);
      renderSyncStatus();
      btn.disabled = false;
      btn.textContent = 'Sign in with Google';
    }
  });

  document.getElementById('sync-sign-out-btn')?.addEventListener('click', async () => {
    try {
      await invoke('sync_sign_out');
      showToast('Signed out. Existing sync data stays in Drive.', 'success');
      loadSyncPage();
      // After sign-out every row becomes foreign (no active account): refresh
      // the lists so the tables no longer show the previous account's rows.
      loadDictionary();
      loadSnippets();
    } catch (err) {
      showToast('Sign-out failed: ' + err, 'error');
    }
  });

  document.getElementById('sync-now-btn')?.addEventListener('click', async () => {
    if (!syncStatus?.enabled) {
      showToast('Enable background sync first', 'error');
      return;
    }
    try {
      const status = await invoke('sync_toggle', { enabled: syncStatus?.enabled ?? false });
      if (status.next_attempt_ms != null) {
        showToast('Sync started', 'success');
      } else {
        showToast('Sync will run shortly', 'success');
      }
    } catch (err) {
      showToast('Failed to start sync: ' + String(err).replace(/^Error:\s*/, ''), 'error');
    }
  });

  window.__TAURI__.event.listen('sync-status', (event) => {
    syncStatus = event.payload;
    renderSyncStatus();
  });
}

// Map a raw backend sync error to safe, user-facing copy. Raw enum/internal
// strings are never rendered; full detail stays in the console for diagnosis.
function describeSyncError(raw) {
  console.log('[sync] last_error detail:', raw);
  const m = String(raw || '').toLowerCase();
  if (/timeout|network|connection|dns/.test(m)) return 'Connection issue. Will retry automatically';
  if (/rate.?limit|quota|too many requests|\b429\b/.test(m)) return 'Google rate limit reached. Pausing briefly';
  if (/rejected|exceeds|too large/.test(m)) return 'Sync data exceeds size limits';
  if (/auth|credential|sign in again/.test(m)) return 'Google Drive access needs reauthorization';
  return 'Sync error. Will retry';
}

function renderSyncStatus() {
  const s = syncStatus || {};
  const enabled = !!s.enabled;
  const signedIn = !!s.signed_in;
  const account = s.account_key || null;

  const cb = document.getElementById('sync-enabled-cb');
  if (cb) cb.checked = enabled;

  const signInBtn = document.getElementById('sync-sign-in-btn');
  const signOutBtn = document.getElementById('sync-sign-out-btn');
  const label = document.getElementById('sync-account-label');
  const desc = document.getElementById('sync-account-desc');
  const statusDesc = document.getElementById('sync-status-desc');
  const nowBtn = document.getElementById('sync-now-btn');

  if (signedIn && account) {
    if (signInBtn) { signInBtn.style.display = 'none'; signInBtn.disabled = false; signInBtn.textContent = 'Sign in with Google'; }
    if (signOutBtn) signOutBtn.style.display = '';
    if (label) label.textContent = account;
    if (desc) desc.textContent = 'Signed in. Your data syncs privately to your personal Drive folder.';
  } else {
    if (account) {
      // PassOutcomeKind::AuthRequired narrowed signed_in to false while the
      // account key remains set: Drive reauthorization (consent revoked,
      // account removed) is genuinely needed. The sign-in button relabels to
      // the one-tap reconnect action.
      if (signInBtn) { signInBtn.style.display = ''; signInBtn.disabled = false; signInBtn.textContent = 'Reconnect Google Drive'; }
      if (label) label.textContent = 'Reconnect Required';
      if (desc) desc.textContent = `Google Drive access needs reauthorization. Reconnect${account ? ' as ' + account : ''} to resume syncing.`;
    } else {
      if (signInBtn) { signInBtn.style.display = ''; signInBtn.disabled = false; signInBtn.textContent = 'Sign in with Google'; }
      if (label) label.textContent = 'Not Signed In';
      if (desc) desc.textContent = 'Sign in with Google to start syncing your data.';
    }
    if (signOutBtn) signOutBtn.style.display = 'none';
  }

  // Persistent sign-in error - visible while signed out (task: under ACCOUNT row, id sync-signin-error)
  let syncSignInErrorEl = document.getElementById('sync-signin-error');
  if (!syncSignInErrorEl) {
    syncSignInErrorEl = document.createElement('div');
    syncSignInErrorEl.id = 'sync-signin-error';
    syncSignInErrorEl.style.color = 'var(--color-error)';
    syncSignInErrorEl.style.fontSize = '12px';
    syncSignInErrorEl.style.lineHeight = '1.4';
    syncSignInErrorEl.style.marginTop = '6px';
    syncSignInErrorEl.setAttribute('role', 'alert');
    const accountInfo = document.getElementById('sync-account-desc')?.parentElement;
    if (accountInfo) accountInfo.appendChild(syncSignInErrorEl);
  }
  if (!signedIn && s.last_error) {
    syncSignInErrorEl.textContent = describeSyncError(s.last_error);
    syncSignInErrorEl.style.display = '';
  } else {
    syncSignInErrorEl.textContent = '';
    syncSignInErrorEl.style.display = 'none';
  }

  if (!enabled) {
    if (statusDesc) statusDesc.textContent = 'Sync is off.';
    if (nowBtn) nowBtn.disabled = !signedIn;
    return;
  }

  const errText = s.last_error ? describeSyncError(s.last_error) : null;

  if (s.running) {
    if (statusDesc) statusDesc.textContent = 'Syncing right now…';
  } else if (s.last_sync_at) {
    // Time-only looks fresh even after days; include the date once the sync
    // is older than today.
    const t = new Date(s.last_sync_at);
    const stamp = t.toDateString() === new Date().toDateString()
      ? t.toLocaleTimeString()
      : t.toLocaleString();
    if (statusDesc) statusDesc.textContent = `Last synced ${stamp}${errText ? ` · ${errText}` : ''}`;
  } else if (errText) {
    if (statusDesc) statusDesc.textContent = errText;
  } else {
    if (statusDesc) statusDesc.textContent = 'Ready to sync.';
  }
  if (nowBtn) nowBtn.disabled = !!s.running || !signedIn;
}

// ── Suggestions (Auto-Learn) ────────────────────────────────────

function setupSuggestions() {
  document.getElementById('dismiss-selected-btn')?.addEventListener('click', dismissSelectedSuggestions);
  document.getElementById('clear-selection-btn')?.addEventListener('click', () => {
    document.querySelectorAll('.suggestion-select:checked').forEach(cb => { cb.checked = false; });
    refreshBulkBar();
  });
  document.getElementById('select-all-suggestions')?.addEventListener('change', (e) => {
    document.querySelectorAll('.suggestion-select').forEach(cb => { cb.checked = e.target.checked; });
    refreshBulkBar();
  });

  // Background UIA monitoring can save a suggestion while this page is open.
  // Refresh only while the dictionary page is visible, avoiding a global poll.
  window.setInterval(() => {
    if (currentPage === 'dictionary' && document.visibilityState === 'visible') {
      loadSuggestions();
    }
  }, 2500);
}

async function loadSuggestions() {
  if (suggestionsLoading) return;
  suggestionsLoading = true;
  try {
    const autoMode = currentSettings?.auto_accept_enabled === true;
    const suggestions = await invoke('get_suggestions');
    const signature = [
      suggestions.length,
      suggestions.map(s => s.id).join(','),
    ].join('|');
    if (signature === lastSuggestionsSignature) return;
    lastSuggestionsSignature = signature;
    renderSuggestionsTable(suggestions, autoMode);
  } catch (err) {
    console.error('Failed to load suggestions:', err);
  } finally {
    suggestionsLoading = false;
  }
}

async function expireStaleSuggestions() {
  try {
    await invoke('expire_stale_suggestions_command');
  } catch (err) {
    console.error('Failed to expire stale suggestions:', err);
  }
}

let lastSuggestionsSignature = null;

const SUGGESTIONS_HINT_MANUAL =
  'Corrections detected from your transcriptions. Accept to add to dictionary, dismiss to ignore.';
const SUGGESTIONS_HINT_AUTO =
  'Auto-accept is on: repeat corrections land straight in the dictionary as auto-added; anything still needing a decision appears here.';

function suggestionRowHtml(s, actionsHtml, seenLabel) {
  return `
    <td class="select-col"><input type="checkbox" class="suggestion-select" data-suggestion-id="${s.id}" aria-label="Select suggestion"></td>
    <td class="spoken-word">${escapeHtml(s.spoken)}</td>
    <td class="corrected-word">${escapeHtml(s.corrected)}</td>
    <td class="col-meta seen-col frequency">${seenLabel}</td>
    <td class="actions">${actionsHtml}</td>
  `;
}

function appendSuggestionRow(tbody, s, preserved) {
  const tr = document.createElement('tr');
  tr.dataset.srow = '1';
  tr.dataset.suggestionId = s.id;
  tr.innerHTML = suggestionRowHtml(s, `
    <button class="btn-ghost btn-small suggestion-accept-btn" data-suggestion-id="${s.id}">Accept</button>
    <button class="btn-ghost btn-small suggestion-dismiss-btn" data-suggestion-id="${s.id}">Dismiss</button>
  `, `${s.frequency}x`);
  tr.querySelector('.suggestion-accept-btn')?.addEventListener('click', () => acceptSuggestion(s.id));
  tr.querySelector('.suggestion-dismiss-btn')?.addEventListener('click', () => dismissSuggestion(s.id));
  tr.querySelector('.suggestion-select')?.addEventListener('change', refreshBulkBar);
  const selectCb = tr.querySelector('.suggestion-select');
  if (selectCb && preserved && preserved.has(s.id)) selectCb.checked = true;
  tbody.appendChild(tr);
}

function renderSuggestionsTable(suggestions, autoMode = false) {
  const tbody = document.getElementById('suggestions-table-body');
  const emptyRow = document.getElementById('suggestions-empty-row');
  const hint = document.getElementById('suggestions-hint');
  if (!tbody) return;

  // Keep the user's selection across a rebuild: the dictionary page polls
  // loadSuggestions in the background, and rebuilding must not drop checks.
  const preserved = new Set(
    [...tbody.querySelectorAll('.suggestion-select:checked')]
      .map(cb => cb.dataset.suggestionId)
      .filter(Boolean)
  );

  if (hint) hint.textContent = autoMode ? SUGGESTIONS_HINT_AUTO : SUGGESTIONS_HINT_MANUAL;

  // Remove all rendered rows; the empty row stays.
  tbody.querySelectorAll('tr[data-srow]').forEach(r => r.remove());

  const showReview = suggestions.length > 0;

  if (!showReview) {
    if (emptyRow) emptyRow.style.display = '';
    refreshBulkBar();
    return;
  }
  if (emptyRow) emptyRow.style.display = 'none';

  suggestions.forEach(s => appendSuggestionRow(tbody, s, preserved));
  refreshBulkBar();
}

function selectedSuggestionIds() {
  return [...document.querySelectorAll('.suggestion-select:checked')]
    .map(cb => cb.dataset.suggestionId)
    .filter(Boolean);
}

function refreshBulkBar() {
  const count = document.getElementById('suggestions-selected-count');
  const ids = selectedSuggestionIds();
  if (count) count.textContent = `${ids.length} selected`;
  const dismiss = document.getElementById('dismiss-selected-btn');
  if (dismiss) dismiss.disabled = ids.length === 0;
  const cancel = document.getElementById('clear-selection-btn');
  if (cancel) cancel.classList.toggle('hidden', ids.length === 0);
  const all = document.getElementById('select-all-suggestions');
  if (all) {
    const boxes = [...document.querySelectorAll('.suggestion-select')];
    all.disabled = boxes.length === 0;
    all.checked = boxes.length > 0 && ids.length === boxes.length;
    all.indeterminate = boxes.length > 0 && ids.length > 0 && ids.length < boxes.length;
  }
}

async function dismissSelectedSuggestions() {
  const ids = selectedSuggestionIds();
  if (ids.length === 0) {
    showToast('Nothing selected', 'error');
    return;
  }
  try {
    for (const id of ids) {
      await invoke('dismiss_suggestion_command', { id });
    }
    showToast(`Dismissed ${ids.length} suggestion${ids.length === 1 ? '' : 's'}`, 'success');
    loadSuggestions();
  } catch (err) {
    showToast('Failed to dismiss selected: ' + err, 'error');
    loadSuggestions();
  }
}

window.acceptSuggestion = async (id) => {
  try {
    await invoke('accept_suggestion_command', { id });
    showToast('Added to dictionary ✓', 'success');
    loadSuggestions();
    loadDictionary();  // Refresh dictionary table too
  } catch (err) {
    showToast('Failed to accept: ' + err, 'error');
  }
};

window.dismissSuggestion = async (id) => {
  try {
    await invoke('dismiss_suggestion_command', { id });
    showToast('Suggestion dismissed', 'success');
    loadSuggestions();
  } catch (err) {
    showToast('Failed to dismiss: ' + err, 'error');
  }
};

function setupUpdaterUI() {
  if (!window.updateManager) return;

  const titleEl = document.getElementById('update-status-title');
  const descEl = document.getElementById('update-status-desc');
  const btnEl = document.getElementById('update-action-btn');
  const lastCheckedEl = document.getElementById('update-last-checked');
  const progressContainer = document.getElementById('update-progress-container');
  const progressFill = document.getElementById('update-progress-fill');
  const progressText = document.getElementById('update-progress-text');

  const sidebarBtn = document.getElementById('sidebar-update-btn');
  const sidebarStatus = document.getElementById('sidebar-update-status');
  const sidebarProgressBar = document.getElementById('sidebar-update-progress-bar');
  const sidebarProgressFill = document.getElementById('sidebar-update-progress-fill');

  let statusTimeout = null;

  window.updateManager.subscribe((info) => {
    // Toggle downloading class on update card for progress bar animation
    const updateCard = document.getElementById('update-card');
    if (updateCard) updateCard.classList.toggle('downloading', info.state === 'downloading');

    // Reset release-note styling unless we're showing the full body below
    descEl?.classList.remove('update-notes');
    if (descEl) descEl.style.textAlign = 'center';

    // Render last checked timestamp
    if (lastCheckedEl) {
      lastCheckedEl.textContent = info.lastCheckedText ? `Last checked: ${info.lastCheckedText}` : '';
    }

    // 1. Update About Page Card (5-State Model)
    if (titleEl && btnEl) {
      switch (info.state) {
        case 'idle':
          titleEl.textContent = "You're up to date";
          descEl.style.display = 'none';
          if (progressContainer) progressContainer.style.display = 'none';
          btnEl.textContent = 'Check for Updates';
          btnEl.disabled = false;
          btnEl.className = 'btn-secondary btn-sm';
          btnEl['onclick'] = () => window.updateManager.checkForUpdates(true);
          break;

        case 'checking':
          titleEl.textContent = 'Checking for updates…';
          descEl.style.display = 'none';
          if (progressContainer) progressContainer.style.display = 'none';
          btnEl.textContent = 'Checking…';
          btnEl.disabled = true;
          btnEl.className = 'btn-secondary btn-sm';
          break;

        case 'available':
          titleEl.textContent = `New Version Available (v${info.version})`;
          descEl.textContent = info.body ? info.body.trim() : 'Bug fixes and performance improvements.';
          descEl.style.display = 'block';
          descEl.style.textAlign = 'left';
          descEl.classList.add('update-notes');
          if (progressContainer) progressContainer.style.display = 'none';
          btnEl.textContent = 'Download Update';
          btnEl.disabled = false;
          btnEl.className = 'btn-primary btn-sm';
          btnEl['onclick'] = () => window.updateManager.startDownloadAndInstall();
          break;

        case 'downloading':
          titleEl.textContent = 'Downloading Update…';
          descEl.style.display = 'none';
          if (progressContainer) {
            progressContainer.style.display = 'block';
            if (progressFill) progressFill.style.width = `${info.downloadProgress}%`;
            if (progressText) progressText.textContent = `${info.downloadProgress}%`;
            document.getElementById('update-progress-track')?.setAttribute('aria-valuenow', Math.round(info.downloadProgress));
          }
          btnEl.textContent = `Downloading ${info.downloadProgress}%`;
          btnEl.disabled = true;
          btnEl.className = 'btn-primary btn-sm';
          break;

        case 'ready':
          titleEl.textContent = 'Update Downloaded & Staged!';
          descEl.textContent = 'Restart Fluence to apply the update.';
          descEl.style.display = 'block';
          if (progressContainer) progressContainer.style.display = 'none';
          btnEl.textContent = 'Restart Fluence';
          btnEl.disabled = false;
          btnEl.className = 'btn-primary btn-sm';
          btnEl['onclick'] = () => window.updateManager.restartApp();
          break;

        case 'failed':
          titleEl.textContent = "Couldn't check for updates";
          descEl.textContent = info.errorMessage || 'Please check your internet connection or try again later.';
          descEl.style.display = 'block';
          if (progressContainer) progressContainer.style.display = 'none';
          btnEl.textContent = 'Try Again';
          btnEl.disabled = false;
          btnEl.className = 'btn-secondary btn-sm';
          btnEl['onclick'] = () => window.updateManager.checkForUpdates(true);
          break;
      }
    }

    // 2. Update Sidebar Widget (Self-explanatory single control)
    const sidebarVersionLabel = document.getElementById('sidebar-version-label');
    if (sidebarBtn && sidebarVersionLabel) {
      const btnText = document.getElementById('sidebar-update-btn-text');

      switch (info.state) {
        case 'idle':
          sidebarVersionLabel.textContent = `v${currentAppVersion}`;
          sidebarVersionLabel.className = 'sidebar-version-label';
          if (btnText) btnText.textContent = 'Check for Updates';
          sidebarBtn.disabled = false;
          sidebarBtn.className = 'sidebar-update-btn';
          sidebarBtn['onclick'] = () => window.updateManager.checkForUpdates(true);
          
          if (sidebarStatus && sidebarStatus.style.display === 'block') {
            sidebarStatus.textContent = '✓ Up to date';
            if (statusTimeout) clearTimeout(statusTimeout);
            statusTimeout = setTimeout(() => {
              if (sidebarStatus) sidebarStatus.style.display = 'none';
            }, 3000);
          } else if (sidebarStatus) {
            sidebarStatus.style.display = 'none';
          }
          if (sidebarProgressBar) sidebarProgressBar.style.display = 'none';
          break;

        case 'checking':
          if (statusTimeout) clearTimeout(statusTimeout);
          sidebarVersionLabel.textContent = `v${currentAppVersion}`;
          sidebarVersionLabel.className = 'sidebar-version-label';
          if (btnText) btnText.textContent = 'Checking…';
          sidebarBtn.disabled = true;
          sidebarBtn.className = 'sidebar-update-btn';
          if (sidebarStatus) {
            sidebarStatus.textContent = 'Checking for updates…';
            sidebarStatus.style.display = 'block';
          }
          if (sidebarProgressBar) sidebarProgressBar.style.display = 'none';
          break;

        case 'available':
          if (statusTimeout) clearTimeout(statusTimeout);
          sidebarVersionLabel.textContent = 'Update Available';
          sidebarVersionLabel.className = 'sidebar-version-label highlight-update';
          if (btnText) btnText.textContent = `Download v${info.version}`;
          sidebarBtn.disabled = false;
          sidebarBtn.className = 'sidebar-update-btn btn-has-update';
          sidebarBtn['onclick'] = () => window.updateManager.startDownloadAndInstall();
          if (sidebarStatus) {
            sidebarStatus.textContent = `v${info.version} ready to download`;
            sidebarStatus.style.display = 'block';
          }
          if (sidebarProgressBar) sidebarProgressBar.style.display = 'none';
          break;

        case 'downloading':
          if (statusTimeout) clearTimeout(statusTimeout);
          sidebarVersionLabel.textContent = 'Downloading Update';
          sidebarVersionLabel.className = 'sidebar-version-label highlight-update';
          if (btnText) btnText.textContent = `Downloading ${info.downloadProgress}%`;
          sidebarBtn.disabled = true;
          sidebarBtn.className = 'sidebar-update-btn';
          if (sidebarStatus) {
            sidebarStatus.textContent = `Downloading ${info.downloadProgress}%`;
            sidebarStatus.style.display = 'block';
          }
          if (sidebarProgressBar) {
            sidebarProgressBar.style.display = 'block';
            sidebarProgressBar.setAttribute('aria-valuenow', Math.round(info.downloadProgress));
            if (sidebarProgressFill) sidebarProgressFill.style.width = `${info.downloadProgress}%`;
          }
          break;

        case 'ready':
          if (statusTimeout) clearTimeout(statusTimeout);
          sidebarVersionLabel.textContent = 'Update Ready';
          sidebarVersionLabel.className = 'sidebar-version-label highlight-ready';
          if (btnText) btnText.textContent = 'Restart Fluence';
          sidebarBtn.disabled = false;
          sidebarBtn.className = 'sidebar-update-btn btn-ready';
          sidebarBtn['onclick'] = () => window.updateManager.restartApp();
          if (sidebarStatus) {
            sidebarStatus.textContent = 'Restart to apply update';
            sidebarStatus.style.display = 'block';
          }
          if (sidebarProgressBar) sidebarProgressBar.style.display = 'none';
          break;

        case 'failed':
          if (statusTimeout) clearTimeout(statusTimeout);
          sidebarVersionLabel.textContent = `v${currentAppVersion}`;
          sidebarVersionLabel.className = 'sidebar-version-label';
          if (btnText) btnText.textContent = 'Try Again';
          sidebarBtn.disabled = false;
          sidebarBtn.className = 'sidebar-update-btn';
          sidebarBtn['onclick'] = () => window.updateManager.checkForUpdates(true);
          if (sidebarStatus) {
            sidebarStatus.textContent = "Couldn't check updates";
            sidebarStatus.style.display = 'block';
          }
          if (sidebarProgressBar) sidebarProgressBar.style.display = 'none';
          break;
      }
    }
  });
}

// ── Keyboard Shortcuts ──────────────────────────────────────────

function setupKeyboardShortcuts() {
  document.addEventListener('keydown', (e) => {
    const target = e.target;
    const isInput = target.tagName === 'INPUT' || target.tagName === 'TEXTAREA' || target.tagName === 'SELECT' || target.isContentEditable;

    // Escape - close window
    if (e.key === 'Escape' && !isInput) {
      e.preventDefault();
      invoke('hide_main_window').catch(() => {});
      return;
    }

    // Ctrl+F / Ctrl+K - focus history search
    if ((e.ctrlKey || e.metaKey) && (e.key === 'f' || e.key === 'k')) {
      e.preventDefault();
      if (currentPage !== 'history') navigateTo('history');
      const searchInput = document.getElementById('history-search');
      if (searchInput) { searchInput.focus(); searchInput.select(); }
      return;
    }

    // Ctrl+S - save current page
    if ((e.ctrlKey || e.metaKey) && e.key === 's') {
      e.preventDefault();
      if (currentPage === 'general') saveGeneral();
      else if (currentPage === 'providers') saveProviders();
      return;
    }
  });
}

// ── Click-to-Copy History Items ─────────────────────────────────

function renderHistoryItem(entry, container) {
  const div = document.createElement('div');
  div.className = 'history-item';
  div.dataset.historyId = entry.id;
  div.tabIndex = 0;
  div.setAttribute('role', 'button');
  div.setAttribute('aria-label', 'Copy transcription to clipboard');

  const date = new Date(entry.timestamp);
  const dayKey = dayKeyFor(date);

  if (container && historyGroupKey !== dayKey) {
    const header = document.createElement('div');
    header.className = 'history-group-header';
    header.dataset.dayKey = dayKey;
    const label = document.createElement('span');
    label.textContent = historyGroupForDate(date);
    const count = document.createElement('span');
    count.className = 'history-group-count';
    header.append(label, count);
    container.appendChild(header);
    historyGroupKey = dayKey;
  }

  const timeStr = formatHistoryTimestamp(entry.timestamp);
  const titleAttr = escapeHtml(date.toLocaleString());
  // History is device-local and not account-scoped (sync_account dropped in v2->v3).
  // Never hide delete behind a hash-vs-email mismatch.
  const foreign = false;

  div.innerHTML = `
    <div class="history-item-header">
      <span class="history-meta-wrap">
        <span class="history-item-time" title="${titleAttr}">${timeStr}</span>
        <span class="history-item-meta">${historyItemMeta(entry)}</span>
      </span>
      <div class="history-actions">
        <span class="badge badge-${entry.mode === 'agent' ? 'primary' : 'success'}">${escapeHtml(entry.mode)}</span>
        ${foreign ? '<span class="badge badge-primary" title="Synced from another account">cloud</span>' : ''}
        <button class="btn-ghost history-copy-btn" style="padding:2px 8px;font-size:11px;">Copy</button>
        ${foreign ? '' : `<button class="btn-ghost history-delete-btn" data-history-id="${entry.id}" aria-label="Delete transcription" style="padding:2px 8px;font-size:11px;color:var(--color-error)">×</button>`}
      </div>
    </div>
    <div class="history-item-text">${renderTranscriptText(entry.text, historySearchQuery)}</div>
  `;

  div.querySelector('.history-copy-btn').addEventListener('click', (e) => {
    e.stopPropagation();
    copyHistoryItem(entry.text, div);
  });
  div.querySelector('.history-delete-btn')?.addEventListener('click', (e) => {
    e.stopPropagation();
    deleteHistoryItem(entry.id);
  });

  div.addEventListener('click', () => copyHistoryItem(entry.text, div));
  div.addEventListener('keydown', (e) => {
    if (e.target !== div) return;
    if (e.key !== 'Enter' && e.key !== ' ') return;
    e.preventDefault();
    copyHistoryItem(entry.text, div);
  });

  container?.appendChild(div);
}

function dayKeyFor(date) {
  return `${date.getFullYear()}-${date.getMonth()}-${date.getDate()}`;
}

// Counts items per rendered day-group from the DOM (no backend call).
// Re-run after every page append so counts stay correct with Load more.
// Also marks the true last item so the timeline rail stops at its dot.
function updateHistoryGroupCounts(list) {
  if (!list) return;
  let lastItem = null;
  list.querySelectorAll('.history-group-header').forEach(header => {
    let n = 0;
    let el = header.nextElementSibling;
    while (el && !el.classList.contains('history-group-header')) {
      if (el.classList.contains('history-item')) { n++; lastItem = el; }
      el = el.nextElementSibling;
    }
    // A header left with zero items (after a delete) shows nothing.
    if (n === 0) {
      header.remove();
      return;
    }
    const countEl = header.querySelector('.history-group-count');
    if (countEl) countEl.textContent = n === 1 ? '1 transcription' : n + ' transcriptions';
  });
  list.querySelectorAll('.history-item.is-last').forEach(el => el.classList.remove('is-last'));
  if (!lastItem) {
    const items = list.querySelectorAll('.history-item');
    lastItem = items.length ? items[items.length - 1] : null;
  }
  if (lastItem) lastItem.classList.add('is-last');
}

// "12 words · 45s" from fields the entry already carries (text +
// duration_ms). Pure derivation — nothing new fetched, nothing synced.
function historyItemMeta(entry) {
  const parts = [];
  const words = String(entry.text || '').trim().split(/\s+/).filter(Boolean).length;
  if (words > 0) parts.push(words === 1 ? '1 word' : words + ' words');
  const ms = Number(entry.duration_ms) || 0;
  if (ms > 0) {
    const s = Math.round(ms / 1000);
    parts.push(s < 60 ? s + 's' : Math.floor(s / 60) + 'm ' + String(s % 60).padStart(2, '0') + 's');
  }
  return escapeHtml(parts.join(' · '));
}

function historyGroupForDate(date) {
  const now = new Date();
  const todayStart = new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime();
  const t = date.getTime();
  if (t >= todayStart) return 'Today';
  if (t >= todayStart - 86400000) return 'Yesterday';
  if (t >= todayStart - 6 * 86400000) {
    return date.toLocaleDateString(undefined, { weekday: 'long' });
  }
  if (date.getFullYear() === now.getFullYear()) {
    return date.toLocaleDateString(undefined, { month: 'long', day: 'numeric' });
  }
  return date.toLocaleDateString(undefined, { year: 'numeric', month: 'long', day: 'numeric' });
}

function formatHistoryTimestamp(ts) {
  const date = new Date(ts);
  const now = new Date();
  const diff = now.getTime() - date.getTime();
  const todayStart = new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime();
  const clock = date.toLocaleTimeString(undefined, { hour: 'numeric', minute: '2-digit' });
  if (diff < 60000) return 'Just now';
  if (diff < 3600000) return `${Math.floor(diff / 60000)}m ago`;
  if (date.getTime() >= todayStart) return `${Math.floor(diff / 3600000)}h ago`;
  if (date.getTime() >= todayStart - 86400000) return `Yesterday, ${clock}`;
  if (date.getTime() >= todayStart - 6 * 86400000) {
    return `${date.toLocaleDateString(undefined, { weekday: 'short' })}, ${clock}`;
  }
  return date.toLocaleDateString(undefined, {
    month: 'short',
    day: 'numeric',
    year: date.getFullYear() === now.getFullYear() ? undefined : 'numeric',
  });
}

function escapeRegExp(str) {
  return str.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

// ── Candidate Word Markers (Pending Suggestions) ────────────────
// Words in history transcripts that match a pending auto-learn
// suggestion are highlighted; clicking the marked word accepts the
// suggestion. The map is cached briefly so history paging stays cheap.

let pendingSuggestionMap = null;
let pendingSuggestionsFetchedAt = 0;

async function loadPendingSuggestionMap(force = false) {
  const now = Date.now();
  if (!force && pendingSuggestionMap && now - pendingSuggestionsFetchedAt < 30000) {
    return pendingSuggestionMap;
  }
  pendingSuggestionMap = new Map();
  pendingSuggestionsFetchedAt = now;
  try {
    const suggestions = await invoke('get_suggestions');
    suggestions.forEach(s => {
      const spoken = s.spoken?.trim();
      if (s.status !== 'pending' || !spoken) return;
      const key = spoken.toLowerCase();
      if (!pendingSuggestionMap.has(key)) {
        pendingSuggestionMap.set(key, { id: s.id, corrected: s.corrected });
      }
    });
  } catch (err) {
    console.error('Failed to load suggestions for markers:', err);
  }
  return pendingSuggestionMap;
}

function renderTranscriptText(text, query) {
  let safe = escapeHtml(text);
  const q = query ? query.trim() : '';
  if (!q && (!pendingSuggestionMap || pendingSuggestionMap.size === 0)) return safe;

  const terms = new Map();
  if (pendingSuggestionMap) {
    pendingSuggestionMap.forEach((info, key) => terms.set(key, { ...info, isCandidate: true }));
  }
  if (q) terms.set(q.toLowerCase(), { isCandidate: false });

  const pattern = [...terms.keys()]
    .sort((a, b) => b.length - a.length)
    .map(escapeRegExp)
    .join('|');
  if (!pattern) return safe;

  const re = new RegExp(`\\b(${pattern})\\b`, 'gi');
  return safe.replace(re, (match) => {
    const info = terms.get(match.toLowerCase());
    if (info.isCandidate) {
      const title = `Suggestion: replace with '${info.corrected}' (click to accept)`;
      return `<mark class="candidate-word" data-suggestion-id="${info.id}" role="button" tabindex="0" title="${escapeHtml(title)}" aria-label="${escapeHtml(title)}">${match}</mark>`;
    }
    return `<mark>${match}</mark>`;
  });
}

window.copyHistoryItem = (text, element) => {
  invoke('copy_text', { text }).then(() => {
    showToast('Copied to clipboard', 'success');
    if (element) {
      element.classList.add('copy-flash');
      setTimeout(() => element.classList.remove('copy-flash'), 400);
    }
  });
};

// ── Skeleton Loading ────────────────────────────────────────────

function setupSkeletonLoading() {
  const statsGrid = document.querySelector('.stats-grid');
  if (statsGrid) {
    statsGrid.querySelectorAll('.stat-card').forEach(card => {
      card.classList.add('skeleton');
    });
  }
}

function removeSkeletonLoading() {
  document.querySelectorAll('.stat-card.skeleton').forEach(card => {
    card.classList.remove('skeleton');
  });
}
