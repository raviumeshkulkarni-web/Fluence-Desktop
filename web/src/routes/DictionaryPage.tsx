import { useCallback, useEffect, useRef, useState } from 'react';
import { BookOpen, Lightbulb } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { Checkbox } from '@/components/ui/checkbox';
import { Field } from '@/components/ui/field';
import { Input } from '@/components/ui/input';
import { Skeleton } from '@/components/ui/skeleton';
import { Switch } from '@/components/ui/switch';
import { toast } from '@/components/fluence/Toasts';
import {
  acceptSuggestion,
  addDictionaryEntry,
  deleteDictionaryEntry,
  dismissSuggestion,
  downloadDictionaryJson,
  expireStaleSuggestions,
  exportDictionaryJson,
  getAutoAccepted,
  getDictionary,
  getSuggestions,
  importDictionaryFile,
  type DictEntry,
  type Suggestion,
} from '@/ipc/dictionary';
import {
  isAutoAccept,
  isAutoLearn,
  loadSettings,
  setAutoLearn,
  setSettingField,
  subscribeSettings,
  getCachedSettings,
} from '@/ipc/settings';
import { useSyncExternalStore } from 'react';

function useSettingsSnapshot() {
  return useSyncExternalStore(subscribeSettings, getCachedSettings);
}

const pairKey = (spoken: string, corrected: string) =>
  spoken.trim().toLowerCase() + '\u0000' + corrected.trim().toLowerCase();

// Faithful port of the vanilla Dictionary surface (#page-dictionary +
// setupDictionary/loadDictionary/renderDictTable/saveDictEntry,
// setupSuggestions/loadSuggestions/renderSuggestionsTable/refreshBulkBar,
// GENERAL_BINDINGS auto-apply for the two learning toggles).
// Same DOM ids/classes, same copy, same toasts, same selection-preservation
// across background poll rebuilds, same 2.5s visible-only refresh.
export function DictionaryPage() {
  const settings = useSettingsSnapshot();
  const learn = isAutoLearn(settings);
  const accept = isAutoAccept(settings);

  const [entries, setEntries] = useState<DictEntry[]>([]);
  const [autoAdded, setAutoAdded] = useState<Set<string>>(new Set());
  const [loading, setLoading] = useState(true);
  const [showAdd, setShowAdd] = useState(false);
  const [spoken, setSpoken] = useState('');
  const [corrected, setCorrected] = useState('');
  const spokenRef = useRef<HTMLInputElement>(null);

  const [suggestions, setSuggestions] = useState<Suggestion[]>([]);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const sigRef = useRef<string | null>(null);

  const loadDict = useCallback(async () => {
    try {
      const list = await getDictionary();
      setEntries(list);
      // Provenance for the "Added / auto" badge (mirrors
      // dictionary::canonical_entry_key); fetched only in auto mode.
      const keys = new Set<string>();
      if (isAutoAccept(getCachedSettings())) {
        try {
          const auto = await getAutoAccepted();
          auto.forEach((s) => keys.add(pairKey(s.spoken, s.corrected)));
        } catch (err) {
          console.error('Failed to load auto-added provenance:', err);
        }
      }
      setAutoAdded(keys);
    } catch (err) {
      toast('Failed to load dictionary: ' + String(err), 'error');
    }
  }, []);

  const loadSugg = useCallback(async () => {
    try {
      const list = await getSuggestions();
      const sig = [list.length, list.map((s) => s.id).join(',')].join('|');
      if (sig === sigRef.current) return;
      sigRef.current = sig;
      setSuggestions(list);
      // Prune selections for rows that no longer exist (vanilla preserves
      // checks across rebuilds the same way).
      setSelected((prev) => {
        const alive = new Set(list.map((s) => s.id));
        const next = new Set([...prev].filter((id) => alive.has(id)));
        return next.size === prev.size ? prev : next;
      });
    } catch (err) {
      console.error('Failed to load suggestions:', err);
    }
  }, []);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        await loadSettings();
      } catch {
        /* toggles keep vanilla defaults until settings arrive */
      }
      if (cancelled) return;
      await loadDict();
      await loadSugg();
      if (!cancelled) setLoading(false);
      void expireStaleSuggestions().catch(() => undefined);
    })();
    const timer = window.setInterval(() => {
      if (document.visibilityState === 'visible') void loadSugg();
    }, 2500);
    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
  }, [loadDict, loadSugg]);

  const openAdd = () => {
    setShowAdd(true);
    requestAnimationFrame(() => spokenRef.current?.focus());
  };
  const closeAdd = () => {
    setShowAdd(false);
    setSpoken('');
    setCorrected('');
  };

  const onSave = async () => {
    const s = spoken.trim();
    const c = corrected.trim();
    if (!s || !c) {
      toast('Please fill in both fields', 'error');
      return;
    }
    try {
      const entry = await addDictionaryEntry(s, c);
      setEntries((prev) => [...prev, entry]);
      closeAdd();
      toast('Entry added', 'success');
    } catch (err) {
      toast('Failed to add entry: ' + String(err), 'error');
    }
  };

  const onDelete = async (id: string) => {
    try {
      await deleteDictionaryEntry(id);
      setEntries((prev) => prev.filter((e) => e.id !== id));
      await loadSugg(); // delete→dismiss linkage may dismiss a suggestion row
      toast('Entry deleted', 'success');
    } catch (err) {
      toast('Failed to delete: ' + String(err), 'error');
    }
  };

  const onImport = async () => {
    try {
      const count = await importDictionaryFile();
      if (count === null) return;
      toast(`Imported ${count} entries`, 'success');
      await loadDict();
    } catch (err) {
      toast('Import failed: ' + String(err), 'error');
    }
  };

  const onExport = async () => {
    try {
      downloadDictionaryJson(await exportDictionaryJson());
    } catch (err) {
      toast('Export failed: ' + String(err), 'error');
    }
  };

  const onAccept = async (id: string) => {
    try {
      await acceptSuggestion(id);
      toast('Added to dictionary', 'success');
      await loadSugg();
      await loadDict();
    } catch (err) {
      toast('Failed to accept: ' + String(err), 'error');
    }
  };

  const onDismiss = async (id: string) => {
    try {
      await dismissSuggestion(id);
      toast('Suggestion dismissed', 'success');
      await loadSugg();
    } catch (err) {
      toast('Failed to dismiss: ' + String(err), 'error');
    }
  };

  const onDismissSelected = async () => {
    if (selected.size === 0) {
      toast('Nothing selected', 'error');
      return;
    }
    try {
      for (const id of selected) await dismissSuggestion(id);
      toast(
        `Dismissed ${selected.size} suggestion${selected.size === 1 ? '' : 's'}`,
        'success',
      );
    } catch (err) {
      toast('Failed to dismiss selected: ' + String(err), 'error');
    } finally {
      setSelected(new Set());
      await loadSugg();
    }
  };

  const toggleSelect = (id: string, on: boolean) =>
    setSelected((prev) => {
      const next = new Set(prev);
      if (on) next.add(id);
      else next.delete(id);
      return next;
    });

  const toggleSelectAll = (on: boolean) =>
    setSelected(on ? new Set(suggestions.map((s) => s.id)) : new Set());

  return (
    <section className="page active" id="page-dictionary">
      <div className="page-header">
        <h1 className="page-title" tabIndex={-1}>Dictionary</h1>
        <p className="page-subtitle">Correct specific words or phrases automatically after transcription</p>
      </div>

      <div className="settings-section">
        <div className="settings-section-header" style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center' }}>
          <h2>Correction Learning</h2>
        </div>
        <div className="setting-row">
          <div className="setting-info">
            <div className="setting-label">Auto-Learn Corrections</div>
            <div className="setting-desc">Suggest transcription corrections based on detected patterns</div>
          </div>
          <div className="setting-control">
            <Field>
              <Switch
                id="auto-learn-cb"
                aria-label="Auto-learn transcription corrections"
                checked={learn}
                onCheckedChange={(v) => setAutoLearn(v)}
              />
            </Field>
          </div>
        </div>
        <div className="setting-row">
          <div className="setting-info">
            <div className="setting-label">Auto-Accept Suggestions</div>
            <div className="setting-desc">Automatically add repeated corrections to the dictionary on this device</div>
          </div>
          <div className="setting-control">
            <Field>
              <Switch
                id="auto-accept-cb"
                aria-label="Auto-accept repeated correction suggestions"
                checked={accept}
                disabled={!learn}
                title={!learn ? 'Requires Auto-Learn' : undefined}
                onCheckedChange={(v) => setSettingField('auto_accept_enabled', v)}
              />
            </Field>
          </div>
        </div>
      </div>

      <div className="settings-section">
        <div className="settings-section-header" style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center' }}>
          <h2>Word Corrections</h2>
          <div style={{ display: 'flex', gap: 'var(--spacing-sm)' }}>
            <Button variant="ghost" size="sm" id="import-dict-btn" onClick={() => void onImport()}>Import</Button>
            <Button variant="ghost" size="sm" id="export-dict-btn" onClick={() => void onExport()}>Export</Button>
            <Button variant="primary" size="sm" id="add-dict-btn" onClick={openAdd}>Add Entry</Button>
          </div>
        </div>
        {showAdd && (
          <div id="dict-add-row">
            <Field label="Spoken Word/Phrase" htmlFor="dict-spoken-input" style={{ flex: 1 }}>
              <Input
                ref={spokenRef}
                type="text"
                id="dict-spoken-input"
                placeholder="e.g. fluence"
                value={spoken}
                onChange={(e) => setSpoken(e.target.value)}
              />
            </Field>
            <Field label="Corrected Form" htmlFor="dict-corrected-input" style={{ flex: 1 }}>
              <Input
                type="text"
                id="dict-corrected-input"
                placeholder="e.g. Fluence"
                value={corrected}
                onChange={(e) => setCorrected(e.target.value)}
              />
            </Field>
            <div style={{ display: 'flex', gap: 'var(--spacing-sm)' }}>
              <Button variant="primary" size="sm" id="dict-save-btn" onClick={() => void onSave()}>Save</Button>
              <Button variant="ghost" size="sm" id="dict-cancel-btn" onClick={closeAdd}>Cancel</Button>
            </div>
          </div>
        )}
        <table className="dict-table" id="dict-table">
          <thead>
            <tr>
              <th className="col-word">Spoken</th>
              <th className="col-word">Corrected</th>
              <th className="col-meta added-col">Added</th>
              <th className="actions">Actions</th>
            </tr>
          </thead>
          <tbody id="dict-table-body">
            {loading && (
              <tr>
                <td colSpan={4}>
                  <div style={{ display: 'flex', flexDirection: 'column', gap: 'var(--spacing-sm)' }}>
                    <Skeleton style={{ height: 40 }} />
                    <Skeleton style={{ height: 40 }} />
                    <Skeleton style={{ height: 40 }} />
                  </div>
                </td>
              </tr>
            )}
            {!loading && entries.length === 0 && (
              <tr id="dict-empty-row">
                <td colSpan={4}>
                  <div className="empty-state">
                    <BookOpen className="empty-state-icon" strokeWidth={1.5} aria-hidden="true" />
                    <div className="empty-state-title">No dictionary entries yet</div>
                    <div className="empty-state-hint">Add corrections for words that are often misheard during transcription</div>
                  </div>
                </td>
              </tr>
            )}
            {!loading && entries.map((entry) => (
              <tr key={entry.id} data-dict-id={entry.id}>
                <td className="spoken-word">{entry.spoken}</td>
                <td className="corrected-word">{entry.corrected}</td>
                <td className="col-meta added-col">
                  {autoAdded.has(pairKey(entry.spoken, entry.corrected)) && (
                    <span className="source-badge">auto</span>
                  )}
                </td>
                <td className="actions">
                  <Button variant="ghost" size="sm" className="dict-delete-btn" data-dict-id={entry.id} onClick={() => void onDelete(entry.id)}>
                    Delete
                  </Button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>

      <div className="settings-section">
        <div className="settings-section-header" style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center' }}>
          <h2>Suggested Corrections</h2>
          <div className="suggestions-bulk-actions" style={{ display: 'flex', alignItems: 'center', gap: 'var(--spacing-sm)' }}>
            <span id="suggestions-selected-count">{selected.size} selected</span>
            <Button variant="ghost" size="sm" id="dismiss-selected-btn" disabled={selected.size === 0} onClick={() => void onDismissSelected()}>
              Dismiss Selected
            </Button>
            {selected.size > 0 && (
              <Button variant="ghost" size="sm" id="clear-selection-btn" onClick={() => setSelected(new Set())}>
                Cancel
              </Button>
            )}
          </div>
        </div>
        <table className="dict-table" id="suggestions-table">
          <thead>
            <tr>
              <th className="select-col">
                <Checkbox
                  id="select-all-suggestions"
                  aria-label="Select all suggestions"
                  disabled={suggestions.length === 0}
                  checked={
                    suggestions.length > 0 && selected.size === suggestions.length
                      ? true
                      : selected.size > 0
                        ? 'indeterminate'
                        : false
                  }
                  onCheckedChange={(v) => toggleSelectAll(v === true)}
                />
              </th>
              <th className="col-word">Detected</th>
              <th className="col-word">Should Be</th>
              <th className="col-meta seen-col">Seen</th>
              <th className="actions">Actions</th>
            </tr>
          </thead>
          <tbody id="suggestions-table-body">
            {loading && (
              <tr>
                <td colSpan={5}>
                  <div style={{ display: 'flex', flexDirection: 'column', gap: 'var(--spacing-sm)' }}>
                    <Skeleton style={{ height: 40 }} />
                    <Skeleton style={{ height: 40 }} />
                    <Skeleton style={{ height: 40 }} />
                  </div>
                </td>
              </tr>
            )}
            {!loading && suggestions.length === 0 && (
              <tr id="suggestions-empty-row">
                <td colSpan={5}>
                  <div className="empty-state">
                    <Lightbulb className="empty-state-icon" strokeWidth={1.5} aria-hidden="true" />
                    <div className="empty-state-title">No suggestions yet</div>
                    <div className="empty-state-hint">Correction suggestions will appear here as you use dictation regularly</div>
                  </div>
                </td>
              </tr>
            )}
            {!loading && suggestions.map((s) => (
              <tr key={s.id} data-srow="1" data-suggestion-id={s.id}>
                <td className="select-col">
                  <Checkbox
                    className="suggestion-select"
                    data-suggestion-id={s.id}
                    aria-label="Select suggestion"
                    checked={selected.has(s.id)}
                    onCheckedChange={(v) => toggleSelect(s.id, v === true)}
                  />
                </td>
                <td className="spoken-word">{s.spoken}</td>
                <td className="corrected-word">{s.corrected}</td>
                <td className="col-meta seen-col frequency">{s.frequency}x</td>
                <td className="actions">
                  <Button variant="ghost" size="sm" className="suggestion-accept-btn" data-suggestion-id={s.id} onClick={() => void onAccept(s.id)}>
                    Accept
                  </Button>
                  <Button variant="ghost" size="sm" className="suggestion-dismiss-btn" data-suggestion-id={s.id} onClick={() => void onDismiss(s.id)}>
                    Dismiss
                  </Button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </section>
  );
}
