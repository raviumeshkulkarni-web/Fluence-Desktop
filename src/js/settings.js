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
let dictEntries = [];

// Provider presets
const STT_PRESETS = {
  groq:    { base_url: 'https://api.groq.com/openai',   model: 'whisper-large-v3' },
  openai:  { base_url: 'https://api.openai.com',        model: 'whisper-1' },
  mistral: { base_url: 'https://api.mistral.ai',        model: 'mistral-stt' },
  custom:  { base_url: '',                              model: '' },
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
  setupSystemToggles();
  setupSaveButtons();
  setupDictionary();
  setupSuggestions();
  populateAudioDevices();
  listenForTauriEvents();
  loadAppVersion();
  loadDashboardStats();

  // Refresh data when window is focused
  window.addEventListener('focus', () => {
    if (currentPage === 'history') {
      loadHistory(true);
      loadDashboardStats();
    }
  });
});

// ── Titlebar ──────────────────────────────────────────────────────

function setupTitlebar() {
  const minimizeBtn = document.getElementById('titlebar-minimize');
  const closeBtn = document.getElementById('titlebar-close');

  if (minimizeBtn) minimizeBtn.addEventListener('click', () => {
    invoke('minimize_main_window').catch(err => console.error('Failed to minimize:', err));
  });
  if (closeBtn) closeBtn.addEventListener('click', () => {
    // Hide instead of close so app stays in tray
    invoke('hide_main_window').catch(err => console.error('Failed to hide:', err));
  });
}

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
  setSelectValue('language-select', s.language || 'en');
  setChecked('autostart-cb', s.auto_start || false);
  setSelectValue('ai-polish-select', s.ai_polish_style || 'none');
  setChecked('auto-grab-cb', s.auto_grab_highlight !== false);
  setChecked('auto-learn-cb', s.auto_learn_enabled !== false);

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

const PAGE_ORDER = ['history', 'general', 'providers', 'dictionary', 'about'];

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
    item.classList.toggle('active', item.dataset.page === page);
  });
  document.querySelectorAll('.page').forEach(p => {
    p.classList.toggle('active', p.id === `page-${page}`);
  });

  // Lazy load data for specific pages
  if (page === 'history') {
    loadHistory(true);
    loadDashboardStats();
  }
  if (page === 'dictionary') {
    loadDictionary();
    loadSuggestions();
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

  document.addEventListener('keyup', () => {
    if (!activeRecorder) return;
    if (pendingHotkey && pendingHotkeyKeys.size > 0) {
      stopHotkeyRecording(true);
    }
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
      startHotkeyRecording(displayId, textId, settingsKey);
    }
  });

  clearBtn?.addEventListener('click', () => {
    setText(textId, defaultShortcut);
    if (currentSettings) currentSettings[settingsKey] = defaultShortcut;
  });
}

function startHotkeyRecording(displayId, textId, settingsKey) {
  const display = document.getElementById(displayId);
  activeRecorder = { displayId, textId, settingsKey, displayEl: display };
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
    if (currentSettings) currentSettings[activeRecorder.settingsKey] = pendingHotkey;
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

  const modifiers = new Set(['Control', 'Alt', 'Shift', 'Meta']);
  if (!modifiers.has(e.key)) {
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
}

function selectProviderCard(type, preset) {
  document.querySelectorAll(`#${type}-provider-grid .provider-card`).forEach(c => {
    c.classList.toggle('selected', c.dataset.provider === preset);
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
}

async function loadHistory(reset, search = '') {
  if (reset) historyPage = 0;

  try {
    const entries = await invoke('get_history', {
      page: historyPage,
      searchQuery: search || null,
    });

    const list = document.getElementById('history-list');
    const emptyEl = document.getElementById('history-empty');

    if (reset && list) {
      list.querySelectorAll('.history-item').forEach(el => el.remove());
    }

    if (entries.length === 0 && historyPage === 0) {
      if (emptyEl) emptyEl.style.display = '';
    } else {
      if (emptyEl) emptyEl.style.display = 'none';
      entries.forEach(entry => renderHistoryItem(entry, list));
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
      setText('stat-total-words', (totalWords / 1000000).toFixed(1) + 'M');
    } else if (totalWords >= 1000) {
      setText('stat-total-words', (totalWords / 1000).toFixed(1) + 'K');
    } else {
      setText('stat-total-words', totalWords.toLocaleString());
    }

    // Time saved from backend stats (estimated typing time: ~40 WPM average)
    const bStats = await invoke('get_history_stats');
    const typingMinutesSaved = totalWords / 40;
    const savedHours = typingMinutesSaved / 60;
    if (savedHours >= 1) {
      setText('stat-time-saved', savedHours.toFixed(1) + 'h');
    } else {
      setText('stat-time-saved', Math.round(typingMinutesSaved) + 'm');
    }

    // Dictation time (actual recording duration)
    const dictationHours = bStats.total_duration_ms / 3600000;
    if (dictationHours >= 1) {
      setText('stat-dictation-time', dictationHours.toFixed(1) + 'h');
    } else {
      setText('stat-dictation-time', Math.round(bStats.total_duration_ms / 60000) + 'm');
    }

    const monthlyHoursSaved = (monthlyWords / 40 / 60);
    if (monthlyHoursSaved >= 1) {
      setText('stat-monthly-saved', monthlyHoursSaved.toFixed(1) + 'h');
    } else {
      setText('stat-monthly-saved', Math.round(monthlyWords / 40) + 'm');
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
        const pct = (dayCounts[i] / maxCount) * 100;
        bar.style.setProperty('--bar-val', pct + '%');
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

function renderHistoryItem(entry, container) {
  const div = document.createElement('div');
  div.className = 'history-item';
  div.dataset.historyId = entry.id;

  const date = new Date(entry.timestamp);
  const timeStr = date.toLocaleString();

  div.innerHTML = `
    <div class="history-item-header">
      <span class="history-item-time">${timeStr}</span>
      <div style="display:flex;gap:6px;align-items:center;">
        <span class="badge badge-${entry.mode === 'agent' ? 'primary' : 'success'}">${escapeHtml(entry.mode)}</span>
        <button class="btn-ghost history-copy-btn" style="padding:2px 8px;font-size:11px;">Copy</button>
        <button class="btn-ghost history-delete-btn" data-history-id="${entry.id}" style="padding:2px 8px;font-size:11px;color:var(--color-error)">×</button>
      </div>
    </div>
    <div class="history-item-text">${escapeHtml(entry.text)}</div>
  `;

  div.querySelector('.history-copy-btn').addEventListener('click', () => copyHistoryItem(entry.text));
  div.querySelector('.history-delete-btn')?.addEventListener('click', () => deleteHistoryItem(entry.id));

  container?.appendChild(div);
}

window.copyHistoryItem = (text) => {
  navigator.clipboard.writeText(text).then(() => showToast('Copied ✓', 'success'));
};

window.deleteHistoryItem = async (id) => {
  try {
    await invoke('delete_history_entry', { id });
    document.querySelector(`[data-history-id="${id}"]`)?.remove();
    showToast('Deleted', 'success');
  } catch (err) {
    showToast('Failed to delete: ' + err, 'error');
  }
};

// ── System Toggles ───────────────────────────────────────────────

function setupSystemToggles() {
  document.getElementById('autostart-cb')?.addEventListener('change', async (e) => {
    try {
      await invoke('set_autostart', { enabled: e.target.checked });
      if (currentSettings) currentSettings.auto_start = e.target.checked;
    } catch (err) {
      showToast('Failed to set autostart: ' + err, 'error');
      e.target.checked = !e.target.checked;
    }
  });
}

// ── Save Buttons ─────────────────────────────────────────────────

function setupSaveButtons() {
  document.getElementById('save-general-btn')?.addEventListener('click', saveGeneral);
  document.getElementById('save-providers-btn')?.addEventListener('click', saveProviders);
}

async function saveGeneral() {
  if (!currentSettings) return;

  currentSettings.hotkey = document.getElementById('hotkey-display-text')?.textContent || currentSettings.hotkey;
  currentSettings.recording_mode = document.getElementById('recording-mode-select')?.value || currentSettings.recording_mode;
  currentSettings.agent_hotkey = document.getElementById('agent-hotkey-display-text')?.textContent || currentSettings.agent_hotkey;
  currentSettings.agent_recording_mode = document.getElementById('agent-recording-mode-select')?.value || currentSettings.agent_recording_mode;
  currentSettings.overlay_position = document.getElementById('overlay-position-select')?.value || currentSettings.overlay_position;
  currentSettings.audio_device_id = document.getElementById('audio-device-select')?.value || null;
  currentSettings.language = document.getElementById('language-select')?.value || 'en';
  currentSettings.sound_on_complete = document.getElementById('sound-on-complete-cb')?.checked ?? false;
  currentSettings.ai_polish_style = document.getElementById('ai-polish-select')?.value || 'none';
  currentSettings.auto_grab_highlight = document.getElementById('auto-grab-cb')?.checked ?? true;
  currentSettings.auto_learn_enabled = document.getElementById('auto-learn-cb')?.checked ?? true;

  try {
    await invoke('update_settings', { settings: currentSettings });
    // Re-register hotkeys with new settings
    await invoke('update_hotkeys', {
      transcriptionShortcut: currentSettings.hotkey,
      transcriptionMode: currentSettings.recording_mode,
      agentShortcut: currentSettings.agent_hotkey,
      agentMode: currentSettings.agent_recording_mode,
    });
    showToast('Settings saved ✓', 'success');
  } catch (err) {
    showToast('Failed to save: ' + err, 'error');
  }
}

async function saveProviders() {
  if (!currentSettings) return;

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

  try {
    await invoke('update_settings', { settings: currentSettings });
    showToast('Provider settings saved ✓', 'success');
  } catch (err) {
    showToast('Failed to save providers: ' + err, 'error');
  }
}

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

async function loadAppVersion() {
  try {
    const version = await invoke('get_app_version');
    setText('version-badge', `v${version}`);
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
  try {
    const isInstalled = await invoke('get_offline_model_status');
    const downloadBtn = document.getElementById('offline-download-btn');
    const deleteBtn = document.getElementById('offline-delete-btn');
    const progressWrapper = document.getElementById('offline-progress-wrapper');
    
    if (isInstalled) {
      if (downloadBtn) {
        downloadBtn.textContent = 'Installed';
        downloadBtn.disabled = true;
      }
      if (deleteBtn) deleteBtn.classList.remove('hidden');
      if (progressWrapper) progressWrapper.classList.add('hidden');
    } else {
      if (downloadBtn) {
        downloadBtn.textContent = 'Download Model';
        downloadBtn.disabled = false;
      }
      if (deleteBtn) deleteBtn.classList.add('hidden');
    }
  } catch (err) {
    console.error('Failed to get offline model status:', err);
  }
}

async function setupOfflineDownloader() {
  const downloadBtn = document.getElementById('offline-download-btn');
  const deleteBtn = document.getElementById('offline-delete-btn');
  const cancelBtn = document.getElementById('offline-cancel-btn');
  const progressWrapper = document.getElementById('offline-progress-wrapper');
  
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

  if (deleteBtn) {
    deleteBtn.addEventListener('click', async () => {
      if (!confirm('Are you sure you want to delete the offline model files to free space (~240 MB)?')) return;
      try {
        const bytesFreed = await invoke('delete_offline_model');
        const mbFreed = (bytesFreed / (1024 * 1024)).toFixed(1);
        showToast(`Offline model files deleted. Freed ${mbFreed} MB ✓`, 'success');
        updateOfflineStatus();
      } catch (err) {
        showToast('Failed to delete model files: ' + err, 'error');
      }
    });
  }

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
      const bytesText = document.getElementById('offline-progress-bytes');
      
      const progress = payload.progress;
      const status = payload.status;
      const currentFile = payload.currentFile;
      
      if (status === 'downloading') {
        if (statusText) statusText.textContent = `Downloading: ${currentFile}`;
        if (percentageText) percentageText.textContent = `${progress.toFixed(0)}%`;
        if (progressFill) progressFill.style.width = `${progress}%`;
        
        const downloadedMb = (payload.bytesDownloaded / (1024 * 1024)).toFixed(1);
        const totalMb = (payload.totalBytes / (1024 * 1024)).toFixed(1);
        if (bytesText) bytesText.textContent = `${downloadedMb} / ${totalMb} MB`;
      } else if (status === 'extracting') {
        if (statusText) statusText.textContent = 'Extracting binaries...';
        if (percentageText) percentageText.textContent = `${progress.toFixed(0)}%`;
        if (progressFill) progressFill.style.width = `${progress}%`;
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
    const { open } = window.__TAURI__.dialog;
    const path = await open({ filters: [{ name: 'JSON', extensions: ['json'] }] });
    if (!path) return;
    const { readTextFile } = window.__TAURI__.fs;
    const json = await readTextFile(path);
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

// ── Suggestions (Auto-Learn) ────────────────────────────────────

function setupSuggestions() {
  document.getElementById('clear-dismissed-btn')?.addEventListener('click', clearDismissedSuggestions);
}

async function loadSuggestions() {
  try {
    const suggestions = await invoke('get_suggestions');
    renderSuggestionsTable(suggestions);
  } catch (err) {
    console.error('Failed to load suggestions:', err);
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
