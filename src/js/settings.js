/**
 * Fluence Windows — Settings Page JavaScript
 * 
 * Handles all settings page interactions:
 * - Navigation between tabs
 * - Hotkey recording
 * - Provider configuration with dynamic model fetching
 * - Transcription history
 * - Auto-start and system toggles
 * 
 * Design: Precision Ink — Clean, matte, monochrome-first
 */

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

// ── State ───────────────────────────────────────────────────────
let currentSettings = null;
let currentPage = 'history';
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
  populateAudioDevices();
  listenForTauriEvents();
  loadAppVersion();
  setupSkeletonLoading();
  loadDashboardStats().finally(removeSkeletonLoading);
  setupUpdaterUI();
  setupKeyboardShortcuts();
  setupSkeletonLoading();

  // Refresh data when window is focused
  window.addEventListener('focus', () => {
    if (currentPage === 'history') {
      loadHistory(true);
      loadDashboardStats();
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
    invoke('hide_main_window').catch(err => console.error('Failed to hide:', err));
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
  setChecked('sound-on-complete-cb', s.sound_on_complete ?? true);
  setSelectValue('offline-engine-select', s.offline_engine || 'sensevoice');

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

const PAGE_ORDER = ['history', 'general', 'providers', 'dictionary', 'snippets', 'about'];

function setupNavigation() {
  document.querySelectorAll('.nav-item').forEach(item => {
    item.addEventListener('click', () => {
      const page = item.dataset.page;
      if (page) navigateTo(page);
    });
  });

  // Listen for tray navigate events
  listen('navigate-to', (evt) => navigateTo(evt.payload));
}

function navigateTo(page) {
  if (currentPage === page) return;

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
  if (page === 'history') {
    loadHistory(true);
    loadDashboardStats();
  }
  if (page === 'dictionary') {
    loadDictionary();
    loadSuggestions();
    expireStaleSuggestions();
  }
  if (page === 'snippets') {
    loadSnippets();
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
      return;
    }

    pendingHotkeyKeys.add(e.key);
    const parts = buildHotkeyString(e);
    setText(activeRecorder.textId, parts || 'Press keys...');
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

  // Keyboard activation (Enter/Space) — while capturing, the document
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
  setText(textId, 'Press your shortcut...');
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

function setupProviderCards() {
  // STT cards
  document.querySelectorAll('#stt-provider-grid .provider-card').forEach(card => {
    card.addEventListener('click', async () => {
      const preset = card.dataset.provider;
      selectProviderCard('stt', preset);
      updateSttUiVisibility(preset);
      if (preset !== 'custom' && preset !== 'Local Offline') {
        setInputValue('stt-base-url', STT_PRESETS[preset]?.base_url || '');
        setSelectOption('stt-model-select', STT_PRESETS[preset]?.model || '');
      }

      // Always clear the key input on switch, but try to fetch the existing key for this provider
      const keyInput = document.getElementById('stt-api-key');
      if (keyInput) keyInput.value = '';

      const target = `Fluence/STT_ApiKey/${preset.toLowerCase().replace(/ /g, '_')}`;
      const hasKey = await invoke('get_api_key', { target }).then(() => true).catch(() => false);
      if (hasKey) {
        fetchModels('stt', true);
      }
      queuePersist('providers');
    });
  });

  // LLM cards
  document.querySelectorAll('#llm-provider-grid .provider-card').forEach(card => {
    card.addEventListener('click', async () => {
      const preset = card.dataset.provider;
      selectProviderCard('llm', preset);
      if (preset !== 'custom') {
        setInputValue('llm-base-url', LLM_PRESETS[preset]?.base_url || '');
        setSelectOption('llm-model-select', LLM_PRESETS[preset]?.model || '');
      }

      const keyInput = document.getElementById('llm-api-key');
      if (keyInput) keyInput.value = '';

      const target = `Fluence/LLM_ApiKey/${preset.toLowerCase().replace(/ /g, '_')}`;
      const hasKey = await invoke('get_api_key', { target }).then(() => true).catch(() => false);
      if (hasKey) {
        fetchModels('llm', true);
      }
      queuePersist('providers');
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
    clearTimeout(sttFetchTimeout);
    sttFetchTimeout = setTimeout(() => fetchModels('stt', true), 800);
  });

  let llmFetchTimeout;
  document.getElementById('llm-api-key')?.addEventListener('input', () => {
    clearTimeout(llmFetchTimeout);
    llmFetchTimeout = setTimeout(() => fetchModels('llm', true), 800);
  });

  // Test buttons
  document.getElementById('stt-test-btn')?.addEventListener('click', () => testConnection('stt'));
  document.getElementById('llm-test-btn')?.addEventListener('click', () => testConnection('llm'));

  // Auto-apply provider endpoint/model changes (debounced)
  document.getElementById('stt-base-url')?.addEventListener('input', () => queuePersist('providers'));
  document.getElementById('llm-base-url')?.addEventListener('input', () => queuePersist('providers'));
  document.getElementById('stt-model-select')?.addEventListener('change', () => queuePersist('providers'));
  document.getElementById('llm-model-select')?.addEventListener('change', () => queuePersist('providers'));
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
    const models = await invoke('fetch_models', { baseUrl, apiKey });
    const select = document.getElementById(`${type}-model-select`);
    if (select) {
      const current = select.value;
      select.textContent = '';
      models.forEach(m => {
        const opt = document.createElement('option');
        opt.value = m;
        opt.textContent = m;
        if (m === current) opt.selected = true;
        select.appendChild(opt);
      });
    }
    if (!silent) showToast(`Loaded ${models.length} models ✓`, 'success');
  } catch (err) {
    if (!silent) showToast('Failed to fetch models: ' + err, 'error');
  } finally {
    if (btn) btn.classList.remove('animate-spin');
  }
}

async function testConnection(type) {
  const baseUrl = document.getElementById(`${type}-base-url`)?.value?.trim();
  const preset = document.querySelector(`#${type}-provider-grid .provider-card.selected`)?.dataset.provider || 'groq';
  const baseTarget = type === 'stt' ? 'Fluence/STT_ApiKey' : 'Fluence/LLM_ApiKey';
  const target = `${baseTarget}/${preset.toLowerCase().replace(/ /g, '_')}`;
  const apiKey = await invoke('get_api_key', { target }).catch(() => '');
  const model = document.getElementById(`${type}-model-select`)?.value || '';  
  const statusDot = document.querySelector(`#${type}-status .dot`);
  const statusText = document.getElementById(`${type}-status-text`);

  if (statusDot) { statusDot.className = 'dot dot-idle'; }
  if (statusText) statusText.textContent = 'Testing...';

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

  document.getElementById('load-more-btn')?.addEventListener('click', () => {
    historyPage++;
    loadHistory(false, document.getElementById('history-search')?.value);
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

    if (reset && list) {
      list.querySelectorAll('.history-item, .history-group-header').forEach(el => el.remove());
      historyGroupKey = null;
    }

    if (entries.length === 0 && historyPage === 0) {
      if (emptyEl) emptyEl.style.display = '';
    } else {
      if (emptyEl) emptyEl.style.display = 'none';
      entries.forEach(entry => renderHistoryItem(entry, list));
      const distinctDays = new Set();
      list?.querySelectorAll('.history-group-header')?.forEach(h => {
        if (h.dataset.dayKey) distinctDays.add(h.dataset.dayKey);
      });
      list?.classList.toggle('single-day', distinctDays.size <= 1);
    }

    const loadMore = document.getElementById('history-load-more');
    if (loadMore) loadMore.classList.toggle('hidden', entries.length < 50);
  } catch (err) {
    showToast('Failed to load history: ' + err, 'error');
  }
}

async function loadDashboardStats() {
  try {
    // Fetch all history entries (loop pages of 50) to compute stats
    let allEntries = [];
    let page = 0;
    while (true) {
      const batch = await invoke('get_history', { page, searchQuery: null });
      allEntries = allEntries.concat(batch);
      if (batch.length < 50) break;
      page++;
    }

    // Total words
    let totalWords = 0;
    let weeklyCount = 0;
    let monthlyCount = 0;
    let weeklyDurationMs = 0;
    let weeklyWords = 0;
    let monthlyWords = 0;
    const now = Date.now();
    const sevenDaysAgo = now - 7 * 86400000;
    const thirtyDaysAgo = now - 30 * 86400000;

    for (const e of allEntries) {
      const words = e.text.split(/\s+/).filter(Boolean).length;
      totalWords += words;
      const ts = new Date(e.timestamp).getTime();
      if (ts >= sevenDaysAgo) { weeklyCount++; weeklyDurationMs += e.duration_ms; weeklyWords += words; }
      if (ts >= thirtyDaysAgo) { monthlyCount++; monthlyWords += words; }
    }

    if (totalWords >= 1000000) {
      animateStatValue('stat-total-words', (totalWords / 1000000).toFixed(1) + 'M');
    } else if (totalWords >= 1000) {
      animateStatValue('stat-total-words', (totalWords / 1000).toFixed(1) + 'K');
    } else {
      animateStatValue('stat-total-words', totalWords.toLocaleString());
    }

    // Time saved from backend stats (estimated typing time: ~40 WPM average)
    const bStats = await invoke('get_history_stats');
    const typingMinutesSaved = totalWords / 40;
    const savedHours = typingMinutesSaved / 60;
    if (savedHours >= 1) {
      animateStatValue('stat-time-saved', savedHours.toFixed(1) + 'h');
    } else {
      animateStatValue('stat-time-saved', Math.round(typingMinutesSaved) + 'm');
    }

    // Dictation time (actual recording duration)
    const dictationHours = bStats.total_duration_ms / 3600000;
    if (dictationHours >= 1) {
      animateStatValue('stat-dictation-time', dictationHours.toFixed(1) + 'h');
    } else {
      animateStatValue('stat-dictation-time', Math.round(bStats.total_duration_ms / 60000) + 'm');
    }

    const monthlyHoursSaved = (monthlyWords / 40 / 60);
    if (monthlyHoursSaved >= 1) {
      animateStatValue('stat-monthly-saved', monthlyHoursSaved.toFixed(1) + 'h');
    } else {
      animateStatValue('stat-monthly-saved', Math.round(monthlyWords / 40) + 'm');
    }

    // Weekly activity bar chart
    const now2 = new Date();
    const dayOfWeek = now2.getDay();
    const mondayOffset = dayOfWeek === 0 ? 6 : dayOfWeek - 1;
    const monday = new Date(now2);
    monday.setDate(now2.getDate() - mondayOffset);
    monday.setHours(0, 0, 0, 0);

    const timestamps = await invoke('get_weekly_activity', { startOfWeekUtc: monday.toISOString() });

    const dayCounts = [0, 0, 0, 0, 0, 0, 0];
    timestamps.forEach(ts => {
      const d = new Date(ts);
      const dow = d.getDay();
      dayCounts[dow === 0 ? 6 : dow - 1]++;
    });

    const maxCount = Math.max(...dayCounts, 1);
    let chartTotal = 0;
    for (let i = 0; i < 7; i++) {
      const bar = document.getElementById(`chart-bar-${i}`);
      const countEl = document.getElementById(`chart-count-${i}`);
      if (bar) {
        const factor = Math.min(1, Math.max(0, dayCounts[i] / maxCount));
        bar.style.transform = `scaleY(${factor})`;
        bar.classList.toggle('animated', dayCounts[i] > 0);
      }
      if (countEl) countEl.textContent = dayCounts[i];
      chartTotal += dayCounts[i];
    }
    const weeklyHoursSaved = (weeklyWords / 40 / 60);
    const weeklyDictationHours = (weeklyDurationMs / 3600000);
    const weeklyWordsLabel = weeklyWords >= 1000 ? (weeklyWords / 1000).toFixed(1) + 'K' : weeklyWords.toLocaleString();
    const weeklySavedLabel = weeklyHoursSaved >= 1 ? weeklyHoursSaved.toFixed(1) + 'h saved' : Math.round(weeklyHoursSaved * 60) + 'm saved';
    const weeklyDictLabel = weeklyDictationHours >= 1 ? weeklyDictationHours.toFixed(1) + 'h spoken' : Math.round(weeklyDictationHours * 60) + 'm spoken';
    setText('chart-header-count', weeklyWordsLabel + ' words · ' + weeklySavedLabel + ' · ' + weeklyDictLabel);
  } catch (err) {
    console.error('Failed to load dashboard stats:', err);
  }
}

window.deleteHistoryItem = async (id) => {
  try {
    await invoke('delete_history_entry', { id });
    document.querySelector(`[data-history-id="${id}"]`)?.remove();
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
  { id: 'offline-engine-select',       key: 'offline_engine',        type: 'select' },
  { id: 'audio-device-select',         key: 'audio_device_id',       type: 'select' },
  { id: 'autostart-cb',                key: 'auto_start',            type: 'checkbox', features: ['autostart'] },
  { id: 'duck-cb',                     key: 'duck_enabled',          type: 'checkbox' },
  { id: 'auto-grab-cb',                key: 'auto_grab_highlight',   type: 'checkbox' },
  { id: 'auto-learn-cb',               key: 'auto_learn_enabled',    type: 'checkbox' },
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
}

function queuePersist(...features) {
  features.forEach(f => persistFeatures.add(f));
  clearTimeout(persistTimer);
  persistTimer = setTimeout(flushPendingPersists, 350);
}

async function flushPendingPersists() {
  if (!currentSettings || persistFeatures.size === 0) return;
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
    await flushPendingPersists();
    showToast('Settings saved ✓', 'success');
  });
  document.getElementById('save-providers-btn')?.addEventListener('click', async () => {
    await flushPendingPersists();
    showToast('Provider settings saved ✓', 'success');
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
    // Audio devices unavailable — silently fail
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
    if (currentPage === 'history') {
      loadHistory(true);
      loadDashboardStats();
    }
  });
}

// ── App Version ──────────────────────────────────────────────────

let currentAppVersion = '1.6.0';

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

  setTimeout(() => {
    toast.style.animation = 'fade-out 0.3s ease forwards';
    setTimeout(() => toast.remove(), 300);
  }, 3000);
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

async function updateOfflineStatus() {
  const engine = document.getElementById('offline-engine-select')?.value || 'sensevoice';
  const svCard = document.getElementById('sensevoice-model-card');
  const msCard = document.getElementById('moonshine-model-card');
  const progressWrapper = document.getElementById('offline-progress-wrapper');

  // Show/hide engine-specific cards
  if (svCard) svCard.style.display = engine === 'sensevoice' ? 'flex' : 'none';
  if (msCard) msCard.style.display = engine === 'moonshine_base' ? 'flex' : 'none';

  try {
    if (engine === 'sensevoice') {
      const isInstalled = await invoke('get_offline_model_status');
      const downloadBtn = document.getElementById('offline-download-btn');
      const deleteBtn = document.getElementById('offline-delete-btn');
      
      if (isInstalled) {
        if (downloadBtn) { downloadBtn.textContent = 'Installed'; downloadBtn.disabled = true; }
        if (deleteBtn) deleteBtn.classList.remove('hidden');
        if (progressWrapper) progressWrapper.classList.add('hidden');
      } else {
        if (downloadBtn) { downloadBtn.textContent = 'Download Model'; downloadBtn.disabled = false; }
        if (deleteBtn) deleteBtn.classList.add('hidden');
      }
    } else {
      const isInstalled = await invoke('get_moonshine_model_status');
      const downloadBtn = document.getElementById('moonshine-download-btn');
      const deleteBtn = document.getElementById('moonshine-delete-btn');
      
      if (isInstalled) {
        if (downloadBtn) { downloadBtn.textContent = 'Installed'; downloadBtn.disabled = true; }
        if (deleteBtn) deleteBtn.classList.remove('hidden');
        if (progressWrapper) progressWrapper.classList.add('hidden');
      } else {
        if (downloadBtn) { downloadBtn.textContent = 'Download Model'; downloadBtn.disabled = false; }
        if (deleteBtn) deleteBtn.classList.add('hidden');
      }
    }
  } catch (err) {
    console.error('Failed to get offline model status:', err);
  }
}

async function setupOfflineDownloader() {
  const downloadBtn = document.getElementById('offline-download-btn');
  const deleteBtn = document.getElementById('offline-delete-btn');
  const moonshineDownloadBtn = document.getElementById('moonshine-download-btn');
  const moonshineDeleteBtn = document.getElementById('moonshine-delete-btn');
  const cancelBtn = document.getElementById('offline-cancel-btn');
  const progressWrapper = document.getElementById('offline-progress-wrapper');
  const engineSelect = document.getElementById('offline-engine-select');
  
  // Engine selector change
  if (engineSelect) {
    engineSelect.addEventListener('change', () => {
      updateOfflineStatus();
    });
  }

  // SenseVoice download button
  if (downloadBtn) {
    downloadBtn.addEventListener('click', async () => {
      try {
        downloadBtn.disabled = true;
        downloadBtn.textContent = 'Connecting...';
        if (progressWrapper) progressWrapper.classList.remove('hidden');
        await invoke('download_offline_model');
      } catch (err) {
        showToast('Failed to start download: ' + err, 'error');
        updateOfflineStatus();
      }
    });
  }

  // SenseVoice delete button
  if (deleteBtn) {
    deleteBtn.addEventListener('click', async () => {
      if (!confirm('Are you sure you want to delete the SenseVoice model files to free space (~240 MB)?')) return;
      try {
        const bytesFreed = await invoke('delete_offline_model');
        const mbFreed = (bytesFreed / (1024 * 1024)).toFixed(1);
        showToast(`SenseVoice model files deleted. Freed ${mbFreed} MB ✓`, 'success');
        updateOfflineStatus();
      } catch (err) {
        showToast('Failed to delete model files: ' + err, 'error');
      }
    });
  }

  // Moonshine download button
  if (moonshineDownloadBtn) {
    moonshineDownloadBtn.addEventListener('click', async () => {
      try {
        moonshineDownloadBtn.disabled = true;
        moonshineDownloadBtn.textContent = 'Connecting...';
        if (progressWrapper) progressWrapper.classList.remove('hidden');
        await invoke('download_moonshine_model');
      } catch (err) {
        showToast('Failed to start download: ' + err, 'error');
        updateOfflineStatus();
      }
    });
  }

  // Moonshine delete button
  if (moonshineDeleteBtn) {
    moonshineDeleteBtn.addEventListener('click', async () => {
      if (!confirm('Are you sure you want to delete the Moonshine Base model files to free space (~287 MB)?')) return;
      try {
        const bytesFreed = await invoke('delete_moonshine_model');
        const mbFreed = (bytesFreed / (1024 * 1024)).toFixed(1);
        showToast(`Moonshine Base model files deleted. Freed ${mbFreed} MB ✓`, 'success');
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
        if (statusText) statusText.textContent = 'Extracting model files...';
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

async function loadDictionary() {
  try {
    dictEntries = await invoke('get_dictionary');
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
      tr.innerHTML = `
        <td class="spoken-word">${escapeHtml(entry.spoken)}</td>
        <td class="corrected-word">${escapeHtml(entry.corrected)}</td>
        <td class="actions">
          <button class="btn-ghost dict-delete-btn" data-dict-id="${entry.id}" style="padding:4px 8px;font-size:12px;color:var(--color-error)">Delete</button>
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

// ── Suggestions (Auto-Learn) ────────────────────────────────────

function setupSuggestions() {
  document.getElementById('clear-dismissed-btn')?.addEventListener('click', clearDismissedSuggestions);

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
    const suggestions = await invoke('get_suggestions');
    renderSuggestionsTable(suggestions);
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

function renderSuggestionsTable(suggestions) {
  const tbody = document.getElementById('suggestions-table-body');
  const emptyRow = document.getElementById('suggestions-empty-row');
  if (!tbody) return;

  // Remove all non-empty rows
  tbody.querySelectorAll('tr[data-suggestion-id]').forEach(r => r.remove());

  if (suggestions.length === 0) {
    if (emptyRow) emptyRow.style.display = '';
  } else {
    if (emptyRow) emptyRow.style.display = 'none';
    suggestions.forEach(s => {
      const tr = document.createElement('tr');
      tr.dataset.suggestionId = s.id;
      tr.innerHTML = `
        <td class="spoken-word">${escapeHtml(s.spoken)}</td>
        <td class="corrected-word">${escapeHtml(s.corrected)}</td>
        <td style="color:var(--color-outline);font-size:12px;">${s.frequency}x</td>
        <td class="actions">
          <button class="btn-ghost suggestion-accept-btn" data-suggestion-id="${s.id}" 
            style="padding:4px 8px;font-size:12px;color:var(--color-success)">Accept</button>
          <button class="btn-ghost suggestion-dismiss-btn" data-suggestion-id="${s.id}" 
            style="padding:4px 8px;font-size:12px;color:var(--color-error)">Dismiss</button>
        </td>
      `;
      tr.querySelector('.suggestion-accept-btn')?.addEventListener('click', () => acceptSuggestion(s.id));
      tr.querySelector('.suggestion-dismiss-btn')?.addEventListener('click', () => dismissSuggestion(s.id));
      tbody.appendChild(tr);
    });
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

async function clearDismissedSuggestions() {
  try {
    await invoke('clear_dismissed_suggestions_command');
    showToast('Cleared dismissed suggestions', 'success');
    loadSuggestions();
  } catch (err) {
    showToast('Failed to clear: ' + err, 'error');
  }
}

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
          btnEl.className = 'btn-secondary';
          btnEl.onclick = () => window.updateManager.checkForUpdates(true);
          break;

        case 'checking':
          titleEl.textContent = 'Checking for updates...';
          descEl.style.display = 'none';
          if (progressContainer) progressContainer.style.display = 'none';
          btnEl.textContent = 'Checking...';
          btnEl.disabled = true;
          btnEl.className = 'btn-secondary';
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
          btnEl.className = 'btn-primary';
          btnEl.onclick = () => window.updateManager.startDownloadAndInstall();
          break;

        case 'downloading':
          titleEl.textContent = 'Downloading Update...';
          descEl.style.display = 'none';
          if (progressContainer) {
            progressContainer.style.display = 'block';
            if (progressFill) progressFill.style.width = `${info.downloadProgress}%`;
            if (progressText) progressText.textContent = `${info.downloadProgress}%`;
            document.getElementById('update-progress-track')?.setAttribute('aria-valuenow', Math.round(info.downloadProgress));
          }
          btnEl.textContent = `Downloading ${info.downloadProgress}%`;
          btnEl.disabled = true;
          btnEl.className = 'btn-primary';
          break;

        case 'ready':
          titleEl.textContent = 'Update Downloaded & Staged!';
          descEl.textContent = 'Restart Fluence to apply the update.';
          descEl.style.display = 'block';
          if (progressContainer) progressContainer.style.display = 'none';
          btnEl.textContent = 'Restart Fluence';
          btnEl.disabled = false;
          btnEl.className = 'btn-primary';
          btnEl.onclick = () => window.updateManager.restartApp();
          break;

        case 'failed':
          titleEl.textContent = "Couldn't check for updates";
          descEl.textContent = info.errorMessage || 'Please check your internet connection or try again later.';
          descEl.style.display = 'block';
          if (progressContainer) progressContainer.style.display = 'none';
          btnEl.textContent = 'Try Again';
          btnEl.disabled = false;
          btnEl.className = 'btn-secondary';
          btnEl.onclick = () => window.updateManager.checkForUpdates(true);
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
          sidebarBtn.onclick = () => window.updateManager.checkForUpdates(true);
          
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
          if (btnText) btnText.textContent = 'Checking...';
          sidebarBtn.disabled = true;
          sidebarBtn.className = 'sidebar-update-btn';
          if (sidebarStatus) {
            sidebarStatus.textContent = 'Checking for updates...';
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
          sidebarBtn.onclick = () => window.updateManager.startDownloadAndInstall();
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
          sidebarBtn.onclick = () => window.updateManager.restartApp();
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
          sidebarBtn.onclick = () => window.updateManager.checkForUpdates(true);
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

    // Escape — close window
    if (e.key === 'Escape' && !isInput) {
      e.preventDefault();
      invoke('hide_main_window').catch(() => {});
      return;
    }

    // Ctrl+F / Ctrl+K — focus history search
    if ((e.ctrlKey || e.metaKey) && (e.key === 'f' || e.key === 'k')) {
      e.preventDefault();
      if (currentPage !== 'history') navigateTo('history');
      const searchInput = document.getElementById('history-search');
      if (searchInput) { searchInput.focus(); searchInput.select(); }
      return;
    }

    // Ctrl+S — save current page
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
    header.textContent = historyGroupForDate(date);
    container.appendChild(header);
    historyGroupKey = dayKey;
  }

  const timeStr = formatHistoryTimestamp(entry.timestamp);
  const titleAttr = escapeHtml(date.toLocaleString());

  div.innerHTML = `
    <div class="history-item-header">
      <span class="history-item-time" title="${titleAttr}">${timeStr}</span>
      <div class="history-actions">
        <span class="badge badge-${entry.mode === 'agent' ? 'primary' : 'success'}">${escapeHtml(entry.mode)}</span>
        <button class="btn-ghost history-copy-btn" style="padding:2px 8px;font-size:11px;">Copy</button>
        <button class="btn-ghost history-delete-btn" data-history-id="${entry.id}" aria-label="Delete transcription" style="padding:2px 8px;font-size:11px;color:var(--color-error)">×</button>
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
      const title = `Suggestion: replace with '${info.corrected}' — click to accept`;
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
