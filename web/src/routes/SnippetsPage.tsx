import { useCallback, useEffect, useRef, useState } from 'react';
import { Zap } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { Field } from '@/components/ui/field';
import { Input } from '@/components/ui/input';
import { Skeleton } from '@/components/ui/skeleton';
import { Switch } from '@/components/ui/switch';
import { toast } from '@/components/fluence/Toasts';
import {
  addSnippet,
  deleteSnippet,
  getSnippets,
  setSnippetsEnabled,
  type Snippet,
} from '@/ipc/snippets';

// Faithful port of the vanilla Snippets surface (#page-snippets +
// setupSnippets/loadSnippets/renderSnippetsTable/saveSnippetEntry).
// Same DOM ids/classes, same copy, same toasts. No polling in vanilla —
// data loads on mount (navigation remounts) plus window-focus refresh.
export function SnippetsPage() {
  const [enabled, setEnabled] = useState(false);
  const [entries, setEntries] = useState<Snippet[]>([]);
  const [loading, setLoading] = useState(true);
  const [showAdd, setShowAdd] = useState(false);
  const [trigger, setTrigger] = useState('');
  const [expansion, setExpansion] = useState('');
  const triggerRef = useRef<HTMLInputElement>(null);

  const load = useCallback(async () => {
    try {
      const store = await getSnippets();
      setEnabled(store.enabled);
      setEntries(store.snippets || []);
    } catch (err) {
      toast('Failed to load snippets: ' + String(err), 'error');
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
    const onFocus = () => void load();
    window.addEventListener('focus', onFocus);
    return () => window.removeEventListener('focus', onFocus);
  }, [load]);

  const openAdd = () => {
    setShowAdd(true);
    requestAnimationFrame(() => triggerRef.current?.focus());
  };
  const closeAdd = () => {
    setShowAdd(false);
    setTrigger('');
    setExpansion('');
  };

  const onToggle = async (on: boolean) => {
    try {
      await setSnippetsEnabled(on);
      setEnabled(on);
      toast(
        on ? 'Text expansion enabled' : 'Text expansion disabled',
        'success',
      );
    } catch (err) {
      toast('Failed to update: ' + String(err), 'error');
    }
  };

  const onSave = async () => {
    const t = trigger.trim();
    const e = expansion.trim();
    if (!t || !e) {
      toast('Please fill in both fields', 'error');
      return;
    }
    try {
      const entry = await addSnippet(t, e);
      setEntries((prev) => [...prev, entry]);
      closeAdd();
      toast('Snippet added', 'success');
    } catch (err) {
      toast(String(err).replace(/^Error:\s*/, ''), 'error');
    }
  };

  const onDelete = async (id: string) => {
    try {
      await deleteSnippet(id);
      setEntries((prev) => prev.filter((s) => s.id !== id));
      toast('Snippet deleted', 'success');
    } catch (err) {
      toast('Failed to delete: ' + String(err), 'error');
    }
  };

  return (
    <section className="page active" id="page-snippets">
      <div className="page-header">
        <h1 className="page-title" tabIndex={-1}>Snippets</h1>
        <p className="page-subtitle">Replace spoken trigger phrases with expansion text in every transcription</p>
      </div>

      <div className="settings-section">
        <div className="setting-row" style={{ paddingBottom: 'var(--spacing-lg)', borderBottom: '1px solid var(--color-outline-variant)' }}>
          <div className="setting-info">
            <div className="setting-label">Enable Text Expansion</div>
            <div className="setting-desc">Dictate a short trigger like &quot;my linkedin&quot; and Fluence pastes your expansion text instead</div>
          </div>
          <div className="setting-control">
            <Field>
              <Switch
                id="snippets-enabled-cb"
                aria-label="Enable text expansion"
                checked={enabled}
                onCheckedChange={(v) => void onToggle(v)}
              />
            </Field>
          </div>
        </div>

        <div className="settings-section-header" style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', marginTop: 'var(--spacing-lg)' }}>
          <h2>My Snippets</h2>
          <Button variant="primary" size="sm" id="add-snippet-btn" onClick={openAdd}>Add Snippet</Button>
        </div>
        {showAdd && (
          <div id="snippet-add-row" style={{ padding: 'var(--spacing-md)', display: 'flex', gap: 'var(--spacing-md)', alignItems: 'flex-end' }}>
            <Field label="Spoken Trigger" htmlFor="snippet-trigger-input" style={{ flex: 1 }}>
              <Input
                ref={triggerRef}
                type="text"
                id="snippet-trigger-input"
                maxLength={100}
                placeholder="e.g. my linkedin"
                value={trigger}
                onChange={(e) => setTrigger(e.target.value)}
              />
            </Field>
            <Field label="Expansion Text" htmlFor="snippet-expansion-input" style={{ flex: 2 }}>
              <Input
                type="text"
                id="snippet-expansion-input"
                maxLength={500}
                placeholder="e.g. https://linkedin.com/in/username"
                value={expansion}
                onChange={(e) => setExpansion(e.target.value)}
              />
            </Field>
            <div style={{ display: 'flex', gap: 'var(--spacing-sm)' }}>
              <Button variant="primary" size="sm" id="snippet-save-btn" onClick={() => void onSave()}>Save</Button>
              <Button variant="ghost" size="sm" id="snippet-cancel-btn" onClick={closeAdd}>Cancel</Button>
            </div>
          </div>
        )}
        <table className="dict-table">
          <thead>
            <tr>
              <th>Trigger</th>
              <th>Expansion</th>
              <th className="actions">Actions</th>
            </tr>
          </thead>
          <tbody id="snippet-table-body">
            {loading && (
              <tr>
                <td colSpan={3}>
                  <div style={{ display: 'flex', flexDirection: 'column', gap: 'var(--spacing-sm)' }}>
                    <Skeleton style={{ height: 40 }} />
                    <Skeleton style={{ height: 40 }} />
                    <Skeleton style={{ height: 40 }} />
                  </div>
                </td>
              </tr>
            )}
            {!loading && entries.length === 0 && (
              <tr id="snippet-empty-row">
                <td colSpan={3}>
                  <div className="empty-state">
                    <Zap className="empty-state-icon" strokeWidth={1.5} aria-hidden="true" />
                    <div className="empty-state-title">No snippets yet</div>
                    <div className="empty-state-hint">Add a trigger phrase and its expansion, e.g. &quot;my email&quot; becomes your full email address</div>
                  </div>
                </td>
              </tr>
            )}
            {!loading && entries.map((entry) => (
              <tr key={entry.id} data-snippet-id={entry.id}>
                <td className="spoken-word">{entry.trigger}</td>
                <td className="corrected-word">{entry.expansion}</td>
                <td className="actions">
                  <Button
                    variant="ghost"
                    size="sm"
                    className="snippet-delete-btn"
                    data-snippet-id={entry.id}
                    style={{ padding: 'var(--spacing-xs) var(--spacing-sm)', fontSize: 'var(--text-label-sm)' }}
                    onClick={() => void onDelete(entry.id)}
                  >
                    Delete
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
