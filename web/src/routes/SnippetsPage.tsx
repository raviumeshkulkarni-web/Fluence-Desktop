import { useCallback, useEffect, useRef, useState } from 'react';
import { Zap } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { Card } from '@/components/ui/card';
import {
  Empty,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from '@/components/ui/empty';
import { Field } from '@/components/ui/field';
import { Input } from '@/components/ui/input';
import { Skeleton } from '@/components/ui/skeleton';
import { Switch } from '@/components/ui/switch';
import {
  Table,
  TableBody,
  TableCell,
  TableCaption,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table';
import { SettingsSectionHeader } from '@/components/fluence/SettingsSection';
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

      <SettingsSectionHeader
        title="Text Expansion"
        description="Configure shorthand triggers that expand into longer text"
      />
      <Card className="settings-card">
        <div className="setting-row">
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
      </Card>

      <SettingsSectionHeader
        title="My Snippets"
        description="Custom triggers and phrases mapped to expanded text"
      >
        <Button variant="default" size="sm" id="add-snippet-btn" onClick={openAdd}>Add Snippet</Button>
      </SettingsSectionHeader>

      {showAdd && (
        <div id="snippet-add-row" className="snippet-add-card snippet-add-form">
          <Field label="Spoken Trigger" htmlFor="snippet-trigger-input" className="snippet-add-field">
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
          <Field label="Expansion Text" htmlFor="snippet-expansion-input" className="snippet-add-field-wide">
            <Input
              type="text"
              id="snippet-expansion-input"
              maxLength={500}
              placeholder="e.g. https://linkedin.com/in/username"
              value={expansion}
              onChange={(e) => setExpansion(e.target.value)}
            />
          </Field>
          <div className="snippet-add-actions">
            <Button variant="default" size="sm" id="snippet-save-btn" onClick={() => void onSave()}>Save</Button>
            <Button variant="ghost" size="sm" id="snippet-cancel-btn" onClick={closeAdd}>Cancel</Button>
          </div>
        </div>
      )}

      <div className="table-card settings-card">
        <Table className="dict-table snippet-table">
          <TableCaption className="sr-only">Saved text expansion snippets</TableCaption>
          <TableHeader>
            <TableRow>
              <TableHead>Trigger</TableHead>
              <TableHead>Expansion</TableHead>
              <TableHead className="actions">Actions</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody id="snippet-table-body">
            {loading && (
              <TableRow>
                <TableCell colSpan={3}>
                  <div className="dict-loading">
                    <Skeleton style={{ height: 40 }} />
                    <Skeleton style={{ height: 40 }} />
                    <Skeleton style={{ height: 40 }} />
                  </div>
                </TableCell>
              </TableRow>
            )}
            {!loading && entries.length === 0 && (
              <TableRow id="snippet-empty-row">
                <TableCell colSpan={3}>
                  <Empty>
                    <EmptyHeader>
                      <EmptyMedia variant="icon">
                        <Zap strokeWidth={1.5} aria-hidden="true" />
                      </EmptyMedia>
                      <EmptyTitle>No snippets yet</EmptyTitle>
                      <EmptyDescription>Add a trigger phrase and its expansion, e.g. &quot;my email&quot; becomes your full email address</EmptyDescription>
                    </EmptyHeader>
                  </Empty>
                </TableCell>
              </TableRow>
            )}
            {!loading && entries.map((entry) => (
              <TableRow key={entry.id} data-snippet-id={entry.id}>
                <TableCell className="spoken-word">{entry.trigger}</TableCell>
                <TableCell className="corrected-word">{entry.expansion}</TableCell>
                <TableCell className="actions">
                  <Button
                    variant="ghost"
                    size="sm"
                    className="agent-delete-btn destructive-action"
                    data-snippet-id={entry.id}
                    onClick={() => void onDelete(entry.id)}
                  >
                    Delete
                  </Button>
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      </div>
    </section>
  );
}
