import { useCallback, useEffect, useState } from 'react';
import { AlertCircle, Zap } from 'lucide-react';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Card, CardContent } from '@/components/ui/card';
import { SettingsSectionHeader } from '@/components/fluence/SettingsSection';
import {
  Empty,
  EmptyContent,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from '@/components/ui/empty';
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog';
import { Field, FieldGroup } from '@/components/ui/field';
import { Input, Textarea } from '@/components/ui/input';
import { RadioGroup, RadioGroupItem } from '@/components/ui/radio-group';
import { Skeleton } from '@/components/ui/skeleton';
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from '@/components/ui/alert-dialog';
import { toast } from '@/components/fluence/Toasts';
import {
  deleteAgent,
  getAgents,
  claimLegacyAgent,
  saveAgent,
  setDefaultAgent,
  type AgentsView,
  type CustomAgent,
} from '@/ipc/agents';
import { describeClaimOutcome } from '@/lib/claim';

// Agents board. Same visual language as Android AgentsScreen: one card
// container, BUILT IN + CUSTOM AGENTS sections, plain rows with dividers.
// The built-in multipurpose agent is fixed. Selecting a row makes it the
// default for Agent Mode; the overlay picker can still override per turn.
export function AgentsPage() {
  const [view, setView] = useState<AgentsView | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [showEditor, setShowEditor] = useState(false);
  const [editing, setEditing] = useState<CustomAgent | null>(null);
  const [name, setName] = useState('');
  const [hint, setHint] = useState('');
  const [saving, setSaving] = useState(false);
  const [pendingDelete, setPendingDelete] = useState<CustomAgent | null>(null);
  const [claimingId, setClaimingId] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      setView(await getAgents());
      setError(null);
    } catch (err) {
      const message = String(err).replace(/^Error:\s*/, '');
      setError(message);
      toast('Failed to load agents: ' + message, 'error');
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

  const retryLoad = () => {
    setLoading(true);
    setError(null);
    void load();
  };

  const onSelectDefault = async (id: string) => {
    try {
      await setDefaultAgent(id);
      setView((prev) => (prev ? { ...prev, default_id: id } : prev));
      toast('Default agent updated', 'success');
    } catch (err) {
      toast(String(err).replace(/^Error:\s*/, ''), 'error');
    }
  };

  const openNew = () => {
    setEditing(null);
    setName('');
    setHint('');
    setShowEditor(true);
  };

  const openEdit = (agent: CustomAgent) => {
    setEditing(agent);
    setName(agent.name);
    setHint(agent.hint);
    setShowEditor(true);
  };

  const onSave = async () => {
    if (!name.trim() || !hint.trim()) {
      toast('Please fill in both name and instructions', 'error');
      return;
    }
    setSaving(true);
    try {
      await saveAgent(
        name.trim(),
        hint.trim(),
        editing?.id,
      );
      await load();
      setShowEditor(false);
      setEditing(null);
      toast('Agent saved', 'success');
    } catch (err) {
      toast(String(err).replace(/^Error:\s*/, ''), 'error');
    } finally {
      setSaving(false);
    }
  };

  const onDelete = async () => {
    if (!pendingDelete) return;
    try {
      await deleteAgent(pendingDelete.id);
      await load();
      toast('Agent deleted', 'success');
    } catch (err) {
      toast(String(err).replace(/^Error:\s*/, ''), 'error');
    } finally {
      setPendingDelete(null);
    }
  };

  // Adopt one unclaimed pre-account agent into the account signed in right now.
  //
  // The button sends only the id — the backend resolves the destination account
  // itself — and it claims exactly this record, never the whole unclaimed set.
  // The message comes from the backend's outcome via the shared pure helper, so
  // a refusal (deleted / duplicated / built-in) explains itself instead of
  // collapsing into a misleading "nothing to add".
  const onClaim = async (agent: CustomAgent) => {
    setClaimingId(agent.id);
    try {
      const outcome = await claimLegacyAgent(agent.id);
      await load();
      const message = describeClaimOutcome(outcome, 'agent');
      toast(message.text, message.kind);
    } catch (err) {
      toast(String(err).replace(/^Error:\s*/, ''), 'error');
    } finally {
      setClaimingId(null);
    }
  };

  if (loading) {
    return (
      <section className="page active" id="page-agents">
        <div className="page-header">
          <h1 className="page-title" tabIndex={-1}>Agents</h1>
        </div>
        <Skeleton className="route-skeleton" />
      </section>
    );
  }

  if (error || !view) {
    return (
      <section className="page active" id="page-agents">
        <div className="page-header">
          <h1 className="page-title" tabIndex={-1}>Agents</h1>
          <p className="page-subtitle">Saved agents are temporarily unavailable.</p>
        </div>
        <Empty className="agents-empty agents-error" role="alert">
          <EmptyHeader>
            <EmptyMedia variant="icon">
              <AlertCircle className="agents-empty-icon" strokeWidth={1.5} aria-hidden="true" />
            </EmptyMedia>
            <EmptyTitle className="agents-empty-title">Unable to load agents</EmptyTitle>
            <EmptyDescription className="agents-empty-desc">
              {error || 'Try again to load your saved agents.'}
            </EmptyDescription>
          </EmptyHeader>
          <EmptyContent>
            <Button variant="default" size="sm" onClick={retryLoad}>
              Try again
            </Button>
          </EmptyContent>
        </Empty>
      </section>
    );
  }

  const customs = view.custom_agents;

    return (
      <section className="page active" id="page-agents">
        <div className="page-header">
          <h1 className="page-title" tabIndex={-1}>Agents</h1>
          <p className="page-subtitle">
            Agents are saved instructions that shape how Agent Mode writes for
            you, e.g. keep my emails short and professional. They only change
            wording and can never take actions. Select one to make it your
            default.
          </p>
        </div>

      <SettingsSectionHeader
        title="Built-in Agent"
        description="The default multipurpose agent that handles general dictation and formatting"
      />
      <Card
        className={`settings-card${view.default_id === view.builtin_id ? ' settings-card-selected' : ''}`}
      >
        <CardContent className="settings-card-content">
          <RadioGroup
            className="agent-choice-group"
            value={view.default_id}
            onValueChange={(id) => void onSelectDefault(id)}
            aria-label="Built-in agents"
          >
            <AgentRow
              value={view.builtin_id}
              title={view.builtin_name}
              description={view.builtin_description}
              isDefault={view.default_id === view.builtin_id}
              selected={view.default_id === view.builtin_id}
              claimable={false}
              claiming={false}
              onClaim={() => {}}
              onSelect={() => void onSelectDefault(view.builtin_id)}
            />
          </RadioGroup>
        </CardContent>
      </Card>

      <SettingsSectionHeader
        title="Custom Agents"
        description="Custom instructions for specific writing styles or workflows"
      >
        <Button
          variant="default"
          size="sm"
          id="agents-new-btn"
          onClick={openNew}
        >
          New Agent
        </Button>
      </SettingsSectionHeader>
      <Card className="settings-card">
        <CardContent className="settings-card-content">
          {customs.length === 0 ? (
          <Empty className="agents-empty">
            <EmptyHeader>
              <EmptyMedia variant="icon">
                <Zap className="agents-empty-icon" strokeWidth={1.5} aria-hidden="true" />
              </EmptyMedia>
              <EmptyTitle className="agents-empty-title">No custom agents yet</EmptyTitle>
              <EmptyDescription className="agents-empty-desc">
                Create one that writes the way you want, e.g. a translator
                that always replies in Hindi
              </EmptyDescription>
            </EmptyHeader>
            <EmptyContent>
              <Button
                variant="secondary"
                size="sm"
                onClick={openNew}
              >
                Create your first agent
              </Button>
            </EmptyContent>
          </Empty>
        ) : (
          <RadioGroup
            className="agent-choice-group"
            value={view.default_id}
            onValueChange={(id) => void onSelectDefault(id)}
            aria-label="Custom agents"
          >
            {customs.map((agent) => (
              <AgentRow
                key={agent.id}
                value={agent.id}
                title={agent.name}
                description={agent.hint}
                isDefault={view.default_id === agent.id}
                selected={view.default_id === agent.id}
                claimable={Boolean(agent.claimable)}
                claiming={claimingId === agent.id}
                onClaim={() => void onClaim(agent)}
                onSelect={() => void onSelectDefault(agent.id)}
                onEdit={() => openEdit(agent)}
                onDelete={() => setPendingDelete(agent)}
              />
            ))}
          </RadioGroup>
        )}
        </CardContent>
      </Card>

      <Dialog
        open={showEditor}
        onOpenChange={(open) => {
          if (!open) setShowEditor(false);
        }}
      >
        <DialogContent aria-label={editing ? 'Edit agent' : 'New agent'}>
          <DialogHeader>
            <DialogTitle>{editing ? 'Edit agent' : 'New agent'}</DialogTitle>
            <DialogDescription>
              {editing
                ? 'Update the instructions used when this agent responds.'
                : 'Create reusable instructions for how this agent should respond.'}
            </DialogDescription>
          </DialogHeader>
          <FieldGroup>
            <Field label="Name (max 30 characters)" htmlFor="agent-name-input">
              <Input
                id="agent-name-input"
                value={name}
                maxLength={30}
                placeholder="e.g. Translator"
                onChange={(e) => setName(e.target.value)}
              />
            </Field>
            <Field
              label="Instructions (max 1000 characters)"
              htmlFor="agent-hint-input"
            >
              <Textarea
                id="agent-hint-input"
                value={hint}
                maxLength={1000}
                rows={4}
                placeholder="e.g. always reply in Hindi, keep it short"
                onChange={(e) => setHint(e.target.value)}
              />
            </Field>
          </FieldGroup>
          <DialogFooter>
            <Button
              variant="secondary"
              onClick={() => setShowEditor(false)}
              disabled={saving}
            >
              Cancel
            </Button>
            <Button onClick={() => void onSave()} disabled={saving}>
              {saving ? 'Saving…' : 'Save'}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <AlertDialog
        open={pendingDelete !== null}
        onOpenChange={(open) => {
          if (!open) setPendingDelete(null);
        }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Delete custom agent</AlertDialogTitle>
            <AlertDialogDescription>
              {pendingDelete
                ? `This action cannot be undone. Delete "${pendingDelete.name}"?`
                : 'This action cannot be undone.'}
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel asChild>
              <Button variant="secondary">Cancel</Button>
            </AlertDialogCancel>
            <AlertDialogAction asChild>
              <Button variant="destructive" onClick={() => void onDelete()}>
                Delete
              </Button>
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </section>
  );
}

function AgentRow({
  value,
  title,
  description,
  isDefault,
  selected,
  claimable,
  claiming,
  onSelect,
  onClaim,
  onEdit,
  onDelete,
}: {
  value: string;
  title: string;
  description: string;
  isDefault: boolean;
  selected: boolean;
  claimable: boolean;
  claiming: boolean;
  onSelect: () => void;
  onClaim: () => void;
  onEdit?: () => void;
  onDelete?: () => void;
}) {
  const isCustom = Boolean(onEdit || onDelete);
  // An unclaimed record is not owned: it cannot be made the default, edited or
  // deleted, because all three act on the account store where it does not exist
  // yet. The only action offered is the explicit claim.
  const hasActions = claimable || !selected || isCustom;

  return (
    <div
      className={`selection-row agent-card${selected ? ' selected' : ''}${hasActions ? ' has-actions' : ''}${claimable ? ' agent-card-unclaimed' : ''}`}
    >
      {claimable ? (
        // Not a RadioGroupItem: an unclaimed agent must never enter the default
        // selection, so it is not offered as a selectable option at all.
        <div className="selection-row-main selection-row-inert">
          <span className="selection-radio" aria-hidden="true" />
          <span className="selection-row-copy">
            <span className="selection-row-heading">
              <span className="selection-row-title">{title}</span>
              <Badge variant="secondary" className="agent-unclaimed-badge">
                On this device
              </Badge>
            </span>
            <span className="selection-row-description agent-row-desc">{description}</span>
            <span className="selection-row-description agent-unclaimed-note">
              Saved on this device before it was linked to an account.
            </span>
          </span>
        </div>
      ) : (
        <RadioGroupItem value={value} asChild>
          <button
            type="button"
            className="selection-row-main"
            aria-label={`Set ${title} as default agent`}
          >
            <span className="selection-radio" aria-hidden="true" />
            <span className="selection-row-copy">
              <span className="selection-row-heading">
                <span className="selection-row-title">{title}</span>
                {isDefault && (
                  <Badge variant="secondary" className="agent-default-badge">
                    DEFAULT
                  </Badge>
                )}
              </span>
              <span className="selection-row-description agent-row-desc">{description}</span>
            </span>
          </button>
        </RadioGroupItem>
      )}
      {hasActions && (
        <div className="selection-row-actions">
          {claimable ? (
            <Button
              variant="secondary"
              size="xs"
              onClick={onClaim}
              disabled={claiming}
            >
              {claiming ? 'Adding…' : 'Add to my account'}
            </Button>
          ) : (
            <>
              {!selected && (
                <Button variant="secondary" size="xs" onClick={onSelect}>
                  Set as default
                </Button>
              )}
              {onEdit && (
                <Button variant="secondary" size="xs" onClick={onEdit}>
                  Edit
                </Button>
              )}
              {onDelete && (
                <Button
                  variant="destructive"
                  size="xs"
                  onClick={onDelete}
                >
                  Delete
                </Button>
              )}
            </>
          )}
        </div>
      )}
    </div>
  );
}
