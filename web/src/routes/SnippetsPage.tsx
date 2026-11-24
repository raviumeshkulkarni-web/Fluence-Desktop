import { useCallback, useEffect, useRef, useState } from 'react';
import { Zap } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { Card, CardContent } from '@/components/ui/card';
import { ConfirmDialog } from '@/components/ui/dialog';
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
import { PageHeader } from '@/components/fluence/PageHeader';
import { RouteErrorBanner } from '@/components/fluence/RouteError';
import { SettingRow } from '@/components/fluence/SettingRow';
import { SettingsSectionHeader } from '@/components/fluence/SettingsSection';
import { toast } from '@/components/fluence/Toasts';
import { describeError } from '@/lib/errors';
import {
  addSnippet,
  deleteSnippet,
  getSnippets,
  setSnippetEnabled,
  setSnippetsEnabled,
  updateSnippet,
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
  const [loadError, setLoadError] = useState<string | null>(null);
  const [pendingDelete, setPendingDelete] = useState<Snippet | null>(null);
  const [showAdd, setShowAdd] = useState(false);
  const [trigger, setTrigger] = useState('');
  const [expansion, setExpansion] = useState('');
  const [editingId, setEditingId] = useState<string | null>(null);
  const [editTrigger, setEditTrigger] = useState('');
  const [editExpansion, setEditExpansion] = useState('');
  const triggerRef = useRef<HTMLInputElement>(null);

  const load = useCallback(async () => {
    try {
      const store = await getSnippets();
      setEnabled(store.enabled);
      setEntries(store.snippets || []);
      setLoadError(null);
    } catch (err) {
      setLoadError(describeError(err, 'Try again.'));
      toast('Could not load snippets. ' + describeError(err), 'error');
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
      toast('Could not update the snippet. ' + describeError(err), 'error');
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
      toast(describeError(err), 'error');
    }
  };

  const onDelete = async (id: string) => {
    try {
      await deleteSnippet(id);
      setEntries((prev) => prev.filter((s) => s.id !== id));
      toast('Snippet deleted', 'success');
    } catch (err) {
      toast('Could not delete the snippet. ' + describeError(err), 'error');
    }
  };

  const openEdit = (entry: Snippet) => {
    setEditingId(entry.id);
    setEditTrigger(entry.trigger);
    setEditExpansion(entry.expansion);
  };
  const closeEdit = () => {
    setEditingId(null);
    setEditTrigger('');
    setEditExpansion('');
  };
  const onSaveEdit = async () => {
    if (!editingId) return;
    const t = editTrigger.trim();
    const e = editExpansion.trim();
    if (!t || !e) {
      toast('Please fill in both fields', 'error');
      return;
    }
    try {
      await updateSnippet(editingId, t, e);
      setEntries((prev) =>
        prev.map((s) =>
          s.id === editingId ? { ...s, trigger: t, expansion: e } : s,
        ),
      );
      closeEdit();
      toast('Snippet updated', 'success');
    } catch (err) {
      toast('Could not update the snippet. ' + describeError(err), 'error');
    }
  };

  const onToggleEntry = async (entry: Snippet, on: boolean) => {
    try {
      await setSnippetEnabled(entry.id, on);
      setEntries((prev) =>
        prev.map((s) => (s.id === entry.id ? { ...s, is_enabled: on } : s)),
      );
    } catch (err) {
      toast('Could not update the snippet. ' + describeError(err), 'error');
    }
  };

  return (
    <section className="page active" id="page-snippets">
      <PageHeader
        title="Snippets"
        subtitle="Replace spoken trigger phrases with expansion text in every transcription"
      />

      <SettingsSectionHeader
        title="Text Expansion"
        description="Configure shorthand triggers that expand into longer text"
      />
      <Card className="settings-card">
        <CardContent className="settings-card-content">
          <SettingRow
            label="Enable Text Expansion"
            htmlFor="snippets-enabled-cb"
            description="Dictate a short trigger like &quot;my linkedin&quot; and Fluence pastes your expansion text instead"
          >
            <Switch
              id="snippets-enabled-cb"
              aria-label="Enable text expansion"
              checked={enabled}
              onCheckedChange={(v) => void onToggle(v)}
            />
          </SettingRow>
        </CardContent>
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
              onKeyDown={(e) => {
                if (e.key === 'Enter') {
                  e.preventDefault();
                  void onSave();
                } else if (e.key === 'Escape') {
                  e.preventDefault();
                  e.stopPropagation();
                  closeAdd();
                }
              }}
            />
          </Field>
          <Field
            label="Expansion Text"
            htmlFor="snippet-expansion-input"
            className="snippet-add-field-wide"
            hint="Use {date}, {time}, {datetime}, {day}, or {year} to insert the current date or time. Placeholders expand on this device; other devices insert the text as-is."
          >
            <Input
              type="text"
              id="snippet-expansion-input"
              maxLength={500}
              placeholder="e.g. https://linkedin.com/in/username"
              value={expansion}
              onChange={(e) => setExpansion(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === 'Enter') {
                  e.preventDefault();
                  void onSave();
                } else if (e.key === 'Escape') {
                  e.preventDefault();
                  e.stopPropagation();
                  closeAdd();
                }
              }}
            />
          </Field>
          <div className="snippet-add-actions">
            <Button variant="default" size="sm" id="snippet-save-btn" onClick={() => void onSave()}>Save</Button>
            <Button variant="ghost" size="sm" id="snippet-cancel-btn" onClick={closeAdd}>Cancel</Button>
          </div>
        </div>
      )}

      {loadError ? (
        <RouteErrorBanner message={loadError} onRetry={() => void load()} />
      ) : null}

      <div className="table-card settings-card">
        <Table className="dict-table snippet-table">
          <TableCaption className="sr-only">Saved text expansion snippets</TableCaption>
          <TableHeader>
            <TableRow>
              <TableHead>Trigger</TableHead>
              <TableHead>Expansion</TableHead>
              <TableHead className="toggles-col">Enabled</TableHead>
              <TableHead className="actions">Actions</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody id="snippet-table-body">
            {loading && (
              <TableRow>
                <TableCell colSpan={4}>
                  <div className="dict-loading">
                    <Skeleton style={{ height: 40 }} />
                    <Skeleton style={{ height: 40 }} />
                    <Skeleton style={{ height: 40 }} />
                  </div>
                </TableCell>
              </TableRow>
            )}
            {!loading && !loadError && entries.length === 0 && (
              <TableRow id="snippet-empty-row">
                <TableCell colSpan={4}>
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
            {!loading && entries.map((entry) => {
              const editing = editingId === entry.id;
              return (
                <TableRow key={entry.id} data-snippet-id={entry.id}>
                  <TableCell className="spoken-word">
                    {editing ? (
                      <Input
                        type="text"
                        maxLength={100}
                        value={editTrigger}
                        aria-label="Edit snippet trigger"
                        onChange={(e) => setEditTrigger(e.target.value)}
                        onKeyDown={(e) => {
                          if (e.key === 'Enter') {
                            e.preventDefault();
                            void onSaveEdit();
                          } else if (e.key === 'Escape') {
                            e.preventDefault();
                            e.stopPropagation();
                            closeEdit();
                          }
                        }}
                      />
                    ) : (
                      entry.trigger
                    )}
                  </TableCell>
                  <TableCell className="corrected-word">
                    {editing ? (
                      <Input
                        type="text"
                        maxLength={500}
                        value={editExpansion}
                        aria-label="Edit snippet expansion"
                        onChange={(e) => setEditExpansion(e.target.value)}
                        onKeyDown={(e) => {
                          if (e.key === 'Enter') {
                            e.preventDefault();
                            void onSaveEdit();
                          } else if (e.key === 'Escape') {
                            e.preventDefault();
                            e.stopPropagation();
                            closeEdit();
                          }
                        }}
                      />
                    ) : (
                      entry.expansion
                    )}
                  </TableCell>
                  <TableCell className="toggles-col">
                    <Switch
                      aria-label={`Enable ${entry.trigger}`}
                      checked={entry.is_enabled !== false}
                      disabled={editing}
                      onCheckedChange={(on) => void onToggleEntry(entry, on)}
                    />
                  </TableCell>
                  <TableCell className="actions">
                    {editing ? (
                      <>
                        <Button variant="default" size="sm" onClick={() => void onSaveEdit()}>
                          Save
                        </Button>
                        <Button variant="ghost" size="sm" onClick={closeEdit}>
                          Cancel
                        </Button>
                      </>
                    ) : (
                      <>
                        <Button variant="ghost" size="sm" onClick={() => openEdit(entry)}>
                          Edit
                        </Button>
                        <Button
                          variant="ghost"
                          size="sm"
                          className="agent-delete-btn destructive-action"
                          data-snippet-id={entry.id}
                          onClick={() => setPendingDelete(entry)}
                        >
                          Delete
                        </Button>
                      </>
                    )}
                  </TableCell>
                </TableRow>
              );
            })}
          </TableBody>
        </Table>
      </div>

      <ConfirmDialog
        open={pendingDelete !== null}
        onOpenChange={(open) => {
          if (!open) setPendingDelete(null);
        }}
        title="Delete snippet"
        body={
          pendingDelete
            ? `This action cannot be undone. Delete "${pendingDelete.trigger}"?`
            : 'This action cannot be undone.'
        }
        confirmLabel="Delete"
        danger
        onConfirm={() => {
          const target = pendingDelete;
          setPendingDelete(null);
          if (target) void onDelete(target.id);
        }}
      />
    </section>
  );
}
